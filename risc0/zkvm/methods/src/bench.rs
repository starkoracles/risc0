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

extern crate alloc;

use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

// Benchmark support structures for communication between host and guest.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum BenchmarkSpec {
    SimpleLoop,
    RawSha {
        buf: Vec<u32>,
    },
    Memcpy {
        src: Vec<u8>,
        src_align: usize,
        dst_align: usize,
    },
    Memset {
        len: usize,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SpecWithIters(pub BenchmarkSpec, pub u64);
