// Copyright 2022 Risc0, Inc.

use std::{collections::HashMap, ffi::c_void, marker::PhantomData, mem, slice};

use metal::{
    Buffer as MetalBuffer, CommandQueue, ComputePipelineDescriptor, Device, MTLResourceOptions,
    MTLSize, NSRange,
};

use super::{Buffer, Hal};
use crate::{
    core::{log2_ceil, sha::Digest},
    field::{
        baby_bear::{BabyBearElem, BabyBearExtElem},
        Elem, ExtElem, RootsOfUnity,
    },
    FRI_FOLD,
};

const METAL_LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kernels.metallib"));

const KERNEL_NAMES: &[&str] = &[
    "batch_expand",
    "eltwise_add_fp",
    "eltwise_copy_fp",
    "eltwise_mul_factor_fp",
    "eltwise_sum_fp4",
    "fri_fold",
    "mix_poly_coeffs",
    "multi_bit_reverse",
    "multi_ntt_fwd_step",
    "multi_ntt_rev_step",
    "multi_poly_eval",
    "sha_fold",
    "sha_rows",
    "zk_shift",
];

#[derive(Debug)]
pub struct MetalHal {
    pub device: Device,
    pub cmd_queue: CommandQueue,
    kernels: HashMap<String, ComputePipelineDescriptor>,
}

#[derive(Clone, Debug)]
pub struct BufferImpl<T> {
    cmd_queue: CommandQueue,
    buffer: MetalBuffer,
    offset: usize,
    size: usize,
    marker: PhantomData<T>,
}

pub enum KernelArg<'a> {
    Buffer {
        buffer: &'a MetalBuffer,
        offset: u64,
    },
    Integer(u32),
}

impl<T> BufferImpl<T> {
    fn new(device: &Device, cmd_queue: CommandQueue, size: usize) -> Self {
        let bytes_len = size * mem::size_of::<T>();
        let options = MTLResourceOptions::StorageModeManaged;
        let buffer = device.new_buffer(bytes_len as u64, options);
        Self {
            cmd_queue,
            buffer,
            offset: 0,
            size,
            marker: PhantomData,
        }
    }

    pub fn copy_from(device: &Device, cmd_queue: CommandQueue, slice: &[T]) -> Self {
        let bytes_len = slice.len() * mem::size_of::<T>();
        let options = MTLResourceOptions::StorageModeManaged;
        let buffer =
            device.new_buffer_with_data(slice.as_ptr() as *const c_void, bytes_len as u64, options);
        Self {
            cmd_queue,
            buffer,
            offset: 0,
            size: slice.len(),
            marker: PhantomData,
        }
    }

    pub fn as_arg<'a>(&'a self) -> KernelArg<'a> {
        let offset = self.offset * mem::size_of::<T>();
        KernelArg::Buffer {
            buffer: &self.buffer,
            offset: offset as u64,
        }
    }

    pub fn as_arg_with_offset<'a>(&'a self, offset: usize) -> KernelArg<'a> {
        let offset = (self.offset + offset) * mem::size_of::<T>();
        KernelArg::Buffer {
            buffer: &self.buffer,
            offset: offset as u64,
        }
    }

    fn sync(&self) {
        let cmd_buffer = self.cmd_queue.new_command_buffer();
        let blit_encoder = cmd_buffer.new_blit_command_encoder();
        blit_encoder.synchronize_resource(&self.buffer);
        blit_encoder.end_encoding();
        cmd_buffer.commit();
        cmd_buffer.wait_until_completed();
    }
}

impl<T: Clone> Buffer<T> for BufferImpl<T> {
    fn size(&self) -> usize {
        self.size
    }

    fn slice(&self, offset: usize, size: usize) -> BufferImpl<T> {
        assert!(offset + size <= self.size());
        BufferImpl {
            cmd_queue: self.cmd_queue.clone(),
            buffer: self.buffer.clone(),
            offset: self.offset + offset,
            size,
            marker: PhantomData,
        }
    }

    fn view<F: FnOnce(&[T])>(&self, f: F) {
        self.sync();
        let ptr = self.buffer.contents() as *const T;
        let len = self.buffer.length() as usize / mem::size_of::<T>();
        let slice = unsafe { slice::from_raw_parts(ptr, len) };
        f(&slice[self.offset..self.offset + self.size]);
    }

    fn view_mut<F: FnOnce(&mut [T])>(&self, f: F) {
        self.sync();
        let ptr = self.buffer.contents() as *mut T;
        let len = self.buffer.length() as usize / mem::size_of::<T>();
        let slice = unsafe { slice::from_raw_parts_mut(ptr, len) };
        f(&mut slice[self.offset..self.offset + self.size]);
        let offset = self.offset * mem::size_of::<T>();
        let size = self.size * mem::size_of::<T>();
        self.buffer
            .did_modify_range(NSRange::new(offset as u64, size as u64));
    }
}

impl MetalHal {
    pub fn new() -> Self {
        let device = Device::system_default().expect("no device found");
        let library = device.new_library_with_data(METAL_LIB).unwrap();
        let cmd_queue = device.new_command_queue();
        let mut kernels = HashMap::new();
        for name in KERNEL_NAMES {
            let function = library.get_function(name, None).unwrap();
            let pipeline = ComputePipelineDescriptor::new();
            pipeline.set_compute_function(Some(&function));
            kernels.insert(name.to_string(), pipeline);
        }
        Self {
            device,
            cmd_queue,
            kernels,
        }
    }

    pub fn dispatch_by_name(&self, name: &str, args: &[KernelArg], count: u64) {
        let kernel = self.kernels.get(name).unwrap();
        self.dispatch(kernel, args, count, None);
    }

    pub fn dispatch(
        &self,
        kernel: &ComputePipelineDescriptor,
        args: &[KernelArg],
        count: u64,
        opts: Option<(MTLSize, MTLSize)>,
    ) {
        let cmd_buffer = self.cmd_queue.new_command_buffer();
        let cmd_encoder = cmd_buffer.new_compute_command_encoder();

        let pipeline_state = self
            .device
            .new_compute_pipeline_state_with_function(kernel.compute_function().unwrap())
            .unwrap();

        cmd_encoder.set_compute_pipeline_state(&pipeline_state);
        for (index, arg) in args.iter().enumerate() {
            match arg {
                KernelArg::Buffer { buffer, offset } => {
                    cmd_encoder.set_buffer(index as u64, Some(buffer), *offset);
                }
                KernelArg::Integer(value) => {
                    cmd_encoder.set_bytes(
                        index as u64,
                        mem::size_of_val(value) as u64,
                        value.to_le_bytes().as_ptr() as *const c_void,
                    );
                }
            }
        }

        match opts {
            Some((thread_groups_count, threads_per_threadgroup)) => {
                cmd_encoder.dispatch_thread_groups(thread_groups_count, threads_per_threadgroup);
            }
            None => {
                let threads_per_grid = MTLSize::new(count, 1, 1);
                let width = pipeline_state.thread_execution_width();
                let height = pipeline_state.max_total_threads_per_threadgroup() / width;
                let threads_per_threadgroup = MTLSize::new(width, height, 1);
                cmd_encoder.dispatch_threads(threads_per_grid, threads_per_threadgroup);
            }
        };

        cmd_encoder.end_encoding();

        cmd_buffer.commit();
        cmd_buffer.wait_until_completed();
    }
}

#[allow(unused_variables)]
impl Hal for MetalHal {
    type Elem = BabyBearElem;
    type ExtElem = BabyBearExtElem;

    type BufferDigest = BufferImpl<Digest>;
    type BufferElem = BufferImpl<Self::Elem>;
    type BufferExtElem = BufferImpl<Self::ExtElem>;
    type BufferU32 = BufferImpl<u32>;

    fn alloc_elem(&self, _name: &'static str, size: usize) -> Self::BufferElem {
        BufferImpl::new(&self.device, self.cmd_queue.clone(), size)
    }

    fn copy_from_elem(&self, _name: &'static str, slice: &[Self::Elem]) -> Self::BufferElem {
        BufferImpl::copy_from(&self.device, self.cmd_queue.clone(), slice)
    }

    fn alloc_extelem(&self, _name: &'static str, size: usize) -> Self::BufferExtElem {
        BufferImpl::new(&self.device, self.cmd_queue.clone(), size)
    }

    fn copy_from_extelem(
        &self,
        _name: &'static str,
        slice: &[Self::ExtElem],
    ) -> Self::BufferExtElem {
        BufferImpl::copy_from(&self.device, self.cmd_queue.clone(), slice)
    }

    fn alloc_digest(&self, _name: &'static str, size: usize) -> Self::BufferDigest {
        BufferImpl::new(&self.device, self.cmd_queue.clone(), size)
    }

    fn copy_from_digest(&self, _name: &'static str, slice: &[Digest]) -> Self::BufferDigest {
        BufferImpl::copy_from(&self.device, self.cmd_queue.clone(), slice)
    }

    fn alloc_u32(&self, _name: &'static str, size: usize) -> Self::BufferU32 {
        BufferImpl::new(&self.device, self.cmd_queue.clone(), size)
    }

    fn copy_from_u32(&self, _name: &'static str, slice: &[u32]) -> Self::BufferU32 {
        BufferImpl::copy_from(&self.device, self.cmd_queue.clone(), slice)
    }

    #[tracing::instrument(skip_all)]
    fn batch_bit_reverse(&self, io: &Self::BufferElem, count: usize) {
        let row_size = io.size() / count;
        assert_eq!(row_size * count, io.size());
        let bits = log2_ceil(row_size);
        assert_eq!(row_size, 1 << bits);
        log::debug!(
            "io: {}, count: {count}, row_size: {row_size}, bits: {bits}",
            io.size()
        );
        let args = &[io.as_arg(), KernelArg::Integer(bits as u32)];
        self.dispatch_by_name("multi_bit_reverse", args, row_size as u64 * count as u64);
    }

    #[tracing::instrument(skip_all)]
    fn batch_expand(&self, output: &Self::BufferElem, input: &Self::BufferElem, poly_count: usize) {
        log::debug!(
            "output: {}, input: {}, poly_count: {poly_count}",
            output.size(),
            input.size()
        );
        let out_size = output.size() / poly_count;
        let in_size = input.size() / poly_count;
        let expand_bits = log2_ceil(out_size / in_size);
        assert_eq!(output.size(), out_size * poly_count);
        assert_eq!(input.size(), in_size * poly_count);
        assert_eq!(out_size, in_size * (1 << expand_bits));
        let args = &[
            output.as_arg(),
            input.as_arg(),
            KernelArg::Integer(poly_count as u32),
            KernelArg::Integer(out_size as u32),
            KernelArg::Integer(in_size as u32),
            KernelArg::Integer(expand_bits as u32),
        ];
        self.dispatch_by_name("batch_expand", args, out_size as u64);
    }

    #[tracing::instrument(skip_all)]
    fn batch_evaluate_ntt(&self, io: &Self::BufferElem, count: usize, expand_bits: usize) {
        log::debug!(
            "io: {}, count: {count}, expand_bits: {expand_bits}",
            io.size()
        );
        let row_size = io.size() / count;
        assert_eq!(row_size * count, io.size());
        let n_bits = log2_ceil(row_size);
        assert_eq!(row_size, 1 << n_bits);
        assert!(n_bits >= expand_bits);
        assert!(n_bits < Self::Elem::MAX_ROU_PO2);
        let rou = self.copy_from_elem("rou", Self::Elem::ROU_FWD);
        let kernel = self.kernels.get("multi_ntt_fwd_step").unwrap();
        for s_bits in 1 + expand_bits..=n_bits {
            let args = &[
                io.as_arg(),
                rou.as_arg(),
                KernelArg::Integer(n_bits as u32),
                KernelArg::Integer(s_bits as u32),
                KernelArg::Integer(count as u32),
            ];
            let params = compute_launch_params(n_bits as u32, s_bits as u32, count as u32);
            self.dispatch(kernel, args, count as u64, Some(params));
        }
    }

    #[tracing::instrument(skip_all)]
    fn batch_interpolate_ntt(&self, io: &Self::BufferElem, count: usize) {
        log::debug!("io: {}, count: {count}", io.size());
        let row_size = io.size() / count;
        assert_eq!(row_size * count, io.size());
        let n_bits = log2_ceil(row_size);
        assert_eq!(row_size, 1 << n_bits);
        assert!(n_bits < Self::Elem::MAX_ROU_PO2);

        let rou = self.copy_from_elem("rou", Self::Elem::ROU_REV);
        let kernel = self.kernels.get("multi_ntt_rev_step").unwrap();
        for s_bits in (1..=n_bits).rev() {
            let args = &[
                io.as_arg(),
                rou.as_arg(),
                KernelArg::Integer(n_bits as u32),
                KernelArg::Integer(s_bits as u32),
                KernelArg::Integer(count as u32),
            ];
            let params = compute_launch_params(n_bits as u32, s_bits as u32, count as u32);
            self.dispatch(kernel, args, count as u64, Some(params));
        }

        let norm = self.copy_from_elem("norm", &[Self::Elem::new(row_size as u32).inv()]);
        let args = &[io.as_arg(), norm.as_arg()];
        self.dispatch_by_name("eltwise_mul_factor_fp", args, io.size() as u64);
    }

    #[tracing::instrument(skip_all)]
    fn batch_evaluate_any(
        &self,
        coeffs: &Self::BufferElem,
        poly_count: usize,
        which: &Self::BufferU32,
        xs: &Self::BufferExtElem,
        out: &Self::BufferExtElem,
    ) {
        let po2 = log2_ceil(coeffs.size() / poly_count);
        let count = 1 << po2;
        assert_eq!(poly_count * count, coeffs.size());
        let eval_count = which.size();
        assert_eq!(xs.size(), eval_count);
        assert_eq!(out.size(), eval_count);
        let args = &[
            out.as_arg(),
            coeffs.as_arg(),
            which.as_arg(),
            xs.as_arg(),
            KernelArg::Integer(count as u32),
        ];
        let kernel = self.kernels.get("multi_poly_eval").unwrap();
        let params = simple_launch_params(out.size() as u32 * 256, 256);
        self.dispatch(kernel, args, count as u64, Some(params));
    }

    #[tracing::instrument(skip_all)]
    fn eltwise_add_elem(
        &self,
        output: &Self::BufferElem,
        input1: &Self::BufferElem,
        input2: &Self::BufferElem,
    ) {
        assert_eq!(output.size(), input1.size());
        assert_eq!(output.size(), input2.size());
        let count = output.size() as u64;
        let args = &[output.as_arg(), input1.as_arg(), input2.as_arg()];
        self.dispatch_by_name("eltwise_add_fp", args, count);
    }

    #[tracing::instrument(skip_all)]
    fn eltwise_sum_extelem(&self, output: &Self::BufferElem, input: &Self::BufferExtElem) {
        let count = output.size() / Self::ExtElem::EXT_SIZE;
        let to_add = input.size() / count;
        assert_eq!(output.size(), count * Self::ExtElem::EXT_SIZE);
        assert_eq!(input.size(), count * to_add);
        let args = &[
            output.as_arg(),
            input.as_arg(),
            KernelArg::Integer(count as u32),
            KernelArg::Integer(to_add as u32),
        ];
        self.dispatch_by_name("eltwise_sum_fp4", args, count as u64);
    }

    #[tracing::instrument(skip_all)]
    fn eltwise_copy_elem(&self, output: &Self::BufferElem, input: &Self::BufferElem) {
        assert_eq!(output.size(), input.size());
        let count = output.size() as u64;
        let args = &[output.as_arg(), input.as_arg()];
        self.dispatch_by_name("eltwise_copy_fp", args, count);
    }

    #[tracing::instrument(skip_all)]
    fn fri_fold(&self, output: &Self::BufferElem, input: &Self::BufferElem, mix: &Self::ExtElem) {
        let count = output.size() / Self::ExtElem::EXT_SIZE;
        assert_eq!(output.size(), count * Self::ExtElem::EXT_SIZE);
        assert_eq!(input.size(), output.size() * FRI_FOLD);
        let mix = self.copy_from_extelem("mix", &[*mix]);
        let args = &[
            output.as_arg(),
            input.as_arg(),
            mix.as_arg(),
            KernelArg::Integer(count as u32),
        ];
        self.dispatch_by_name("fri_fold", args, count as u64);
    }

    #[tracing::instrument(skip_all)]
    fn mix_poly_coeffs(
        &self,
        output: &Self::BufferExtElem,
        mix_start: &Self::ExtElem,
        mix: &Self::ExtElem,
        input: &Self::BufferElem,
        combos: &Self::BufferU32,
        input_size: usize,
        count: usize,
    ) {
        log::debug!(
            "output: {}, input: {}, combos: {}, input_size: {input_size}, count: {count}",
            output.size(),
            input.size(),
            combos.size(),
        );

        let mix_start = self.copy_from_extelem("mix_start", &[*mix_start]);
        let mix = self.copy_from_extelem("mix", &[*mix]);
        let args = &[
            output.as_arg(),
            input.as_arg(),
            combos.as_arg(),
            mix_start.as_arg(),
            mix.as_arg(),
            KernelArg::Integer(input_size as u32),
            KernelArg::Integer(count as u32),
        ];
        self.dispatch_by_name("mix_poly_coeffs", args, count as u64);
    }

    fn sha_fold(&self, io: &Self::BufferDigest, input_size: usize, output_size: usize) {
        assert_eq!(input_size, 2 * output_size);
        let args = &[
            io.as_arg_with_offset(output_size),
            io.as_arg_with_offset(input_size),
        ];
        self.dispatch_by_name("sha_fold", args, output_size as u64);
    }

    #[tracing::instrument(skip_all)]
    fn sha_rows(&self, output: &Self::BufferDigest, matrix: &Self::BufferElem) {
        let row_size = output.size();
        let col_size = matrix.size() / output.size();
        assert_eq!(matrix.size(), col_size * row_size);
        let args = &[
            output.as_arg(),
            matrix.as_arg(),
            KernelArg::Integer(row_size as u32),
            KernelArg::Integer(col_size as u32),
        ];
        self.dispatch_by_name("sha_rows", args, row_size as u64);
    }

    #[tracing::instrument(skip_all)]
    fn zk_shift(&self, io: &Self::BufferElem, poly_count: usize) {
        let bits = log2_ceil(io.size() / poly_count);
        let count = io.size();
        assert_eq!(io.size(), poly_count * (1 << bits));
        let args = &[io.as_arg(), KernelArg::Integer(bits as u32)];
        self.dispatch_by_name("zk_shift", args, count as u64);
    }
}

fn simple_launch_params(count: u32, threads_per_group: u32) -> (MTLSize, MTLSize) {
    let groups = (count + threads_per_group - 1) / threads_per_group;
    (
        MTLSize::new(groups.into(), 1, 1),
        MTLSize::new(threads_per_group.into(), 1, 1),
    )
}

fn compute_launch_params(n_bits: u32, s_bits: u32, c_size: u32) -> (MTLSize, MTLSize) {
    let s_size = 1 << (s_bits - 1);
    let g_size = 1 << (n_bits - s_bits);

    let mut grid = MTLSize::new(1, 1, 1);
    let mut block = MTLSize::new(1, 1, 1);

    let mut threads = 128;
    // First thread over S
    block.width = threads.min(s_size);
    threads /= block.width;
    // Next thread over G
    block.height = threads.min(g_size);
    // Don't bother threading over C
    let mut grids = 32;
    // First grid over S
    grid.width = grids.min(s_size / block.width);
    grids /= grid.width;
    // Next grid over G
    grid.height = grids.min(g_size / block.height);
    grids /= grid.height;
    // Next grid over C
    grid.depth = grids.min(c_size as u64);
    (grid, block)
}

#[cfg(test)]
mod tests {
    use test_log::test;

    use super::MetalHal;
    use crate::hal::testutil;

    #[test]
    fn batch_bit_reverse() {
        testutil::batch_bit_reverse(MetalHal::new());
    }

    #[test]
    fn batch_evaluate_any() {
        testutil::batch_evaluate_any(MetalHal::new());
    }

    #[test]
    fn batch_evaluate_ntt() {
        testutil::batch_evaluate_ntt(MetalHal::new());
    }

    #[test]
    fn batch_expand() {
        testutil::batch_expand(MetalHal::new());
    }

    #[test]
    fn batch_interpolate_ntt() {
        testutil::batch_interpolate_ntt(MetalHal::new());
    }

    #[test]
    #[should_panic]
    fn check_req() {
        testutil::check_req(MetalHal::new());
    }

    #[test]
    fn eltwise_add_fp() {
        testutil::eltwise_add_elem(MetalHal::new());
    }

    #[test]
    fn eltwise_copy_fp() {
        testutil::eltwise_copy_elem(MetalHal::new());
    }

    #[test]
    fn eltwise_sum_extelem() {
        testutil::eltwise_sum_extelem(MetalHal::new());
    }

    #[test]
    fn fri_fold() {
        testutil::fri_fold(MetalHal::new());
    }

    #[test]
    fn mix_poly_coeffs() {
        testutil::mix_poly_coeffs(MetalHal::new());
    }

    #[test]
    fn sha_fold() {
        testutil::sha_fold(MetalHal::new());
    }

    #[test]
    fn sha_rows() {
        testutil::sha_rows(MetalHal::new());
    }

    #[test]
    fn slice() {
        testutil::slice(MetalHal::new());
    }

    #[test]
    fn zk_shift() {
        testutil::zk_shift(MetalHal::new());
    }
}
