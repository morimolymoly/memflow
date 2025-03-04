/*!
memflow is a library that allows live memory introspection of running systems and their snapshots.
Due to its modular approach it trivial to support almost any scenario where Direct Memory Access is available.

The very core of the library is a [PhysicalMemory](mem/phys_mem/index.html) that provides direct memory access in an abstract environment.
This object that can be defined both statically, and pluginsally with the use of the `dynamic` feature.
If `plugins` is enabled, it is possible to dynamically load libraries that provide Direct Memory Access.

Through the use of OS abstraction layers, like [memflow-win32](https://github.com/memflow/memflow-win32),
user can gain access to virtual memory of individual processes,
by creating objects that implement [VirtualMemory](mem/virt_mem/index.html).

Bridging the two is done by a highly throughput optimized virtual address translation function,
which allows for crazy fast memory transfers at scale.

The core is architecture independent (as long as addresses fit in 64-bits), and currently both 32,
and 64-bit versions of the x86 family are available to be used.

For non-rust libraries, it is possible to use the [FFI](https://github.com/memflow/memflow/tree/master/memflow-ffi)
to interface with the library.

You will almost always import this module when working with memflow.
*/

#![cfg_attr(not(feature = "std"), no_std)]
extern crate no_std_compat as std;

#[macro_use]
extern crate bitflags;

#[macro_use]
extern crate smallvec;

pub mod error;

#[macro_use]
pub mod types;

pub mod architecture;

pub mod mem;

pub mod connector;

#[cfg(feature = "plugins")]
pub mod plugins;

pub mod os;

pub mod iter;

// forward declare
#[doc(hidden)]
pub mod derive {
    pub use ::memflow_derive::*;
}

#[doc(hidden)]
pub mod cglue {
    pub use ::cglue::prelude::v1::*;
}

#[doc(hidden)]
#[cfg(feature = "abi_stable")]
pub mod abi_stable {
    pub use ::abi_stable::*;
}

#[doc(hidden)]
pub mod dataview {
    pub use ::dataview::*;
    pub use ::memflow_derive::Pod;
}

#[doc(hidden)]
#[cfg(any(feature = "dummy_mem", test))]
pub mod dummy;

#[doc(hidden)]
pub mod prelude {
    pub mod v1 {
        pub use crate::architecture::*;
        pub use crate::cglue::*;
        pub use crate::connector::*;
        pub use crate::dataview::*;
        pub use crate::derive::*;
        pub use crate::error::*;
        pub use crate::iter::*;
        pub use crate::mem::*;
        pub use crate::os::*;
        #[cfg(feature = "plugins")]
        pub use crate::plugins::os::*;
        #[cfg(feature = "plugins")]
        pub use crate::plugins::*;
        pub use crate::types::*;
    }
    pub use v1::*;
}
