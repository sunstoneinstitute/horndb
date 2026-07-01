//! SIMD kernel selection metrics (SPEC-12). Emitted once at server startup:
//! one `1`-valued series per `horndb-simd` primitive on the `(kernel, isa,
//! source)` the startup selection chose — `source` records *which* path picked
//! it (known-CPU table / calibration / static). The `SimdKernel`/`SimdIsa`/
//! `SimdSource` label types are metrics-local so this crate need not depend on
//! `horndb-simd`; the server binary maps `horndb_simd`'s own types onto them at
//! the emit site.

use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;

use crate::labels::{SimdIsa, SimdKernel, SimdKernelLabel, SimdSource};

#[derive(Clone)]
pub struct SimdMetrics {
    pub kernel_isa: Family<SimdKernelLabel, Gauge>,
}

impl SimdMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let kernel_isa = Family::<SimdKernelLabel, Gauge>::default();
        reg.register(
            "simd_kernel_isa",
            "Selected SIMD ISA per horndb-simd primitive (1 on the active kernel/ISA series)",
            kernel_isa.clone(),
        );
        Self { kernel_isa }
    }

    /// Mark `(kernel, isa, source)` as the chosen kernel by setting its series
    /// to 1. `source` records which selection path picked this `(kernel, isa)`.
    pub fn record(&self, kernel: SimdKernel, isa: SimdIsa, source: SimdSource) {
        self.kernel_isa
            .get_or_create(&SimdKernelLabel {
                kernel,
                isa,
                source,
            })
            .set(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_encodes_simd_series() {
        let mut reg = Registry::with_prefix("horndb");
        let m = SimdMetrics::register(&mut reg);
        m.record(
            SimdKernel::Intersect,
            SimdIsa::Avx512,
            SimdSource::Calibrated,
        );
        m.record(SimdKernel::LowerBound, SimdIsa::Scalar, SimdSource::Table);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(
            buf.contains(
                "horndb_simd_kernel_isa{kernel=\"intersect\",isa=\"avx512\",source=\"calibrated\"} 1"
            ),
            "got:\n{buf}"
        );
        assert!(
            buf.contains(
                "horndb_simd_kernel_isa{kernel=\"lower_bound\",isa=\"scalar\",source=\"table\"} 1"
            ),
            "got:\n{buf}"
        );
    }
}
