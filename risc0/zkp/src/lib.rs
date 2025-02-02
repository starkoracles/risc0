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
#![deny(rustdoc::broken_intra_doc_links)]

extern crate alloc;

pub mod adapter;
pub mod core;
pub mod field;
#[cfg(not(target_os = "zkvm"))]
pub mod hal;
mod merkle;
#[cfg(not(target_os = "zkvm"))]
pub mod prove;
pub mod taps;
pub mod verify;

pub const MIN_CYCLES_PO2: usize = 10;
pub const MIN_CYCLES: usize = 1 << MIN_CYCLES_PO2; // 1K
pub const MAX_CYCLES_PO2: usize = 24;
pub const MAX_CYCLES: usize = 1 << MAX_CYCLES_PO2; // 16M

/// ~100 bits of conjectured security
pub const QUERIES: usize = 50;
pub const ZK_CYCLES: usize = QUERIES;
pub const MIN_PO2: usize = core::log2_ceil(1 + ZK_CYCLES);

pub const INV_RATE: usize = 4;
const FRI_FOLD_PO2: usize = 4;
pub const FRI_FOLD: usize = 1 << FRI_FOLD_PO2;
const FRI_MIN_DEGREE: usize = 256;
