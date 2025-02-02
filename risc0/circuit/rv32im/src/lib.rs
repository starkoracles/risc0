// Copyright 2022 Risc0, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(target_os = "zkvm"))]
mod cpp;
#[cfg(not(target_os = "zkvm"))]
pub mod cpu;
#[cfg(feature = "cuda")]
pub mod cuda;
#[cfg(not(target_os = "zkvm"))]
mod ffi;
mod info;
#[cfg(all(target_os = "macos", not(feature = "verify-only")))]
pub mod metal;
mod poly_ext;
mod taps;
pub mod verify_taps_rv32im;

use risc0_zkp::{adapter::TapsProvider, taps::TapSet};

pub struct CircuitImpl;

impl CircuitImpl {
    pub const fn new() -> Self {
        CircuitImpl
    }
}

impl TapsProvider for CircuitImpl {
    fn get_taps(&self) -> &'static TapSet<'static> {
        taps::TAPSET
    }
}

#[cfg(test)]
mod tests {
    use risc0_zkp::{
        adapter::{CircuitStep, CircuitStepContext, CircuitStepHandler},
        field::baby_bear::BabyBearElem,
    };

    use crate::CircuitImpl;

    struct CustomStepMock {}

    impl CircuitStepHandler<BabyBearElem> for CustomStepMock {
        fn call(
            &mut self,
            _cycle: usize,
            name: &str,
            extra: &str,
            args: &[BabyBearElem],
            outs: &mut [BabyBearElem],
        ) -> anyhow::Result<()> {
            println!("name: {name}, extra: {extra}, args: {args:?}");
            outs[0] = BabyBearElem::new(2);
            Ok(())
        }

        fn sort(&mut self, _name: &str) {
            unimplemented!()
        }
        fn calc_prefix_products(&mut self) {
            unimplemented!()
        }
    }

    #[test]
    fn step_exec() {
        let circuit = CircuitImpl::new();
        let mut custom = CustomStepMock {};
        let ctx = CircuitStepContext { size: 0, cycle: 0 };
        let mut args0 = vec![BabyBearElem::default(); 20];
        let mut args2 = vec![BabyBearElem::default(); 20];
        let args: &mut [&mut [BabyBearElem]] =
            &mut [&mut args0, &mut [], &mut args2, &mut [], &mut []];
        circuit.step_exec(&ctx, &mut custom, args).unwrap();
    }
}

#[cfg(feature = "test")]
pub mod testutil {
    use rand::{thread_rng, Rng};
    use risc0_zkp::{
        adapter::{CircuitInfo, TapsProvider},
        field::{
            baby_bear::{BabyBearElem, BabyBearExtElem},
            Elem, ExtElem,
        },
        hal::{Buffer, EvalCheck, Hal},
        taps::RegisterGroup,
        INV_RATE,
    };

    use crate::CircuitImpl;

    pub struct EvalCheckParams {
        pub po2: usize,
        pub steps: usize,
        pub domain: usize,
        pub code: Vec<BabyBearElem>,
        pub data: Vec<BabyBearElem>,
        pub accum: Vec<BabyBearElem>,
        pub mix: Vec<BabyBearElem>,
        pub out: Vec<BabyBearElem>,
        pub poly_mix: BabyBearExtElem,
    }

    impl EvalCheckParams {
        pub fn new(po2: usize) -> Self {
            let mut rng = thread_rng();
            let steps = 1 << po2;
            let domain = steps * INV_RATE;
            let circuit = crate::CircuitImpl::new();
            let taps = circuit.get_taps();
            let code_size = taps.group_size(RegisterGroup::Code);
            let data_size = taps.group_size(RegisterGroup::Data);
            let accum_size = taps.group_size(RegisterGroup::Accum);
            let code = random_fps(&mut rng, code_size * domain);
            let data = random_fps(&mut rng, data_size * domain);
            let accum = random_fps(&mut rng, accum_size * domain);
            let mix = random_fps(&mut rng, CircuitImpl::MIX_SIZE);
            let out = random_fps(&mut rng, CircuitImpl::OUTPUT_SIZE);
            let poly_mix = BabyBearExtElem::random(&mut rng);
            log::debug!("code: {} bytes", code.len() * 4);
            log::debug!("data: {} bytes", data.len() * 4);
            log::debug!("accum: {} bytes", accum.len() * 4);
            log::debug!("mix: {} bytes", mix.len() * 4);
            log::debug!("out: {} bytes", out.len() * 4);
            Self {
                po2,
                steps,
                domain,
                code,
                data,
                accum,
                mix,
                out,
                poly_mix,
            }
        }
    }

    fn random_fps<E: Elem>(rng: &mut impl Rng, size: usize) -> Vec<E> {
        let mut ret = Vec::new();
        for _ in 0..size {
            ret.push(E::random(rng));
        }
        ret
    }

    #[allow(unused)]
    pub(crate) fn eval_check<H1, H2, E1, E2>(hal1: &H1, eval1: E1, hal2: &H2, eval2: E2, po2: usize)
    where
        H1: Hal<Elem = BabyBearElem, ExtElem = BabyBearExtElem>,
        H2: Hal<Elem = BabyBearElem, ExtElem = BabyBearExtElem>,
        E1: EvalCheck<H1>,
        E2: EvalCheck<H2>,
    {
        let params = EvalCheckParams::new(po2);
        let check1 = eval_check_impl(&params, hal1, &eval1);
        let check2 = eval_check_impl(&params, hal2, &eval2);
        assert_eq!(check1, check2);
    }

    pub fn eval_check_impl<H, E>(params: &EvalCheckParams, hal: &H, eval: &E) -> Vec<H::Elem>
    where
        H: Hal<Elem = BabyBearElem, ExtElem = BabyBearExtElem>,
        E: EvalCheck<H>,
    {
        let check = hal.alloc_elem("check", BabyBearExtElem::EXT_SIZE * params.domain);
        let code = hal.copy_from_elem("code", &params.code);
        let data = hal.copy_from_elem("data", &params.data);
        let accum = hal.copy_from_elem("accum", &params.accum);
        let mix = hal.copy_from_elem("mix", &params.mix);
        let out = hal.copy_from_elem("out", &params.out);
        eval.eval_check(
            &check,
            &code,
            &data,
            &accum,
            &mix,
            &out,
            params.poly_mix,
            params.po2,
            params.steps,
        );
        let mut ret = vec![H::Elem::ZERO; check.size()];
        check.view(|view| {
            ret.clone_from_slice(view);
        });
        ret
    }
}
