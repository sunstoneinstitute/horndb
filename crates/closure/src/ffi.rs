//! Raw FFI bindings to SuiteSparse:GraphBLAS, generated at build time by bindgen.
//!
//! Do not call these directly outside the `grb` safe-wrapper module.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]
#![allow(unnecessary_transmutes)]
#![allow(clippy::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
