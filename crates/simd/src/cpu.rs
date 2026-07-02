//! Host CPU detection and the known-CPU kernel table.
//!
//! Real SPB-256 measurements proved the micro-calibrated SIMD kernels are net
//! *harmful* versus scalar on the CPUs we run on (both AMD Zen4 and Intel
//! Sapphire Rapids): the balanced, L2-resident calibration inputs are
//! unrepresentative of the skewed, memory-bound shapes production dispatches. So
//! before falling back to calibration, each primitive consults an authoritative
//! per-CPU table keyed on the host's CPUID vendor/family/model. A table hit
//! selects a kernel with **no timing**; a miss falls through to calibration.
//!
//! The table is intentionally per-`(cpu, kernel)`: today both known rows pin
//! *every* kernel to scalar, but the shape supports future per-kernel SIMD
//! entries without a signature change.
//!
//! On non-x86_64 hosts there is no accessible CPUID, so [`detect`] returns
//! `None` and every primitive falls through to calibration.

use crate::dispatch::Isa;
use std::sync::OnceLock;

/// CPU vendor, decoded from the CPUID leaf-0 vendor string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Vendor {
    Intel,
    Amd,
    Other,
}

/// A host CPU identity: vendor plus the CPUID display family/model (the
/// extended-arithmetic values, not the raw base nibbles).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CpuKey {
    pub vendor: Vendor,
    pub family: u32,
    pub model: u32,
}

/// The primitives this crate dispatches. `name` returns the stable
/// [`crate::calibration_report`] string so downstream logging/label use is
/// unaffected by this enum's introduction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kernel {
    Intersect,
    LowerBound,
    Merge,
    Dedup,
    FilterRange,
    FilterIndicesEq,
    Gather,
}

impl Kernel {
    /// The stable, human-readable name of this kernel, matching the strings in
    /// [`crate::calibration_report`].
    pub fn name(self) -> &'static str {
        match self {
            Kernel::Intersect => "intersect",
            Kernel::LowerBound => "lower_bound",
            Kernel::Merge => "merge",
            Kernel::Dedup => "dedup",
            Kernel::FilterRange => "filter_range",
            Kernel::FilterIndicesEq => "filter_indices_eq",
            Kernel::Gather => "gather",
        }
    }
}

/// Decode a CPUID leaf-1 `EAX` word into `(display_family, display_model)` using
/// the standard extended arithmetic. Pure and testable without CPUID.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
fn decode_family_model(eax: u32) -> (u32, u32) {
    let base_family = (eax >> 8) & 0xF;
    let base_model = (eax >> 4) & 0xF;
    let display_family = if base_family == 0xF {
        base_family + ((eax >> 20) & 0xFF)
    } else {
        base_family
    };
    let display_model = if base_family == 0x6 || base_family == 0xF {
        base_model + (((eax >> 16) & 0xF) << 4)
    } else {
        base_model
    };
    (display_family, display_model)
}

/// Decode the CPUID leaf-0 vendor registers `(EBX, EDX, ECX)` into a [`Vendor`].
/// The 12-byte vendor string is `EBX ++ EDX ++ ECX` in little-endian byte order.
/// Pure and testable without CPUID.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
fn decode_vendor(ebx: u32, edx: u32, ecx: u32) -> Vendor {
    let mut s = [0u8; 12];
    s[0..4].copy_from_slice(&ebx.to_le_bytes());
    s[4..8].copy_from_slice(&edx.to_le_bytes());
    s[8..12].copy_from_slice(&ecx.to_le_bytes());
    match &s {
        b"GenuineIntel" => Vendor::Intel,
        b"AuthenticAMD" => Vendor::Amd,
        _ => Vendor::Other,
    }
}

/// Read the host CPU identity via CPUID (x86_64) or report `None` where CPUID is
/// inaccessible (non-x86_64). Memoised.
pub(crate) fn detect() -> Option<CpuKey> {
    static CACHE: OnceLock<Option<CpuKey>> = OnceLock::new();
    *CACHE.get_or_init(detect_uncached)
}

#[cfg(target_arch = "x86_64")]
fn detect_uncached() -> Option<CpuKey> {
    // Safety: `__cpuid` is always available on x86_64 (CPUID has been baseline
    // since the mid-90s); it reads no memory and has no preconditions.
    let (vendor, eax) = unsafe {
        use std::arch::x86_64::__cpuid;
        let leaf0 = __cpuid(0);
        let vendor = decode_vendor(leaf0.ebx, leaf0.edx, leaf0.ecx);
        let leaf1 = __cpuid(1);
        (vendor, leaf1.eax)
    };
    let (family, model) = decode_family_model(eax);
    Some(CpuKey {
        vendor,
        family,
        model,
    })
}

#[cfg(not(target_arch = "x86_64"))]
fn detect_uncached() -> Option<CpuKey> {
    // No accessible CPUID (e.g. aarch64): fall through to calibration.
    None
}

/// The authoritative kernel choice for a known host CPU, or `None` for an
/// unknown CPU (or a known CPU with no entry for this kernel). Structured as
/// `(vendor, family, model)` → per-[`Kernel`] choice so future per-kernel SIMD
/// entries slot in without a signature change.
pub(crate) fn table_pick(cpu: CpuKey, k: Kernel) -> Option<Isa> {
    match (cpu.vendor, cpu.family, cpu.model) {
        // AMD Zen4 (Ryzen 7 7700). SPB-256: scalar 36.0 vs calibrated 28.6 qps
        // — every calibrated SIMD kernel loses, so pin all to scalar.
        (Vendor::Amd, 25, 97) => Some(scalar_all(k)),
        // Intel Sapphire Rapids (Xeon Gold 5412U). SPB-256: scalar 34.4 vs
        // all-AVX2 17.3 qps — every SIMD kernel loses, so pin all to scalar.
        (Vendor::Intel, 6, 143) => Some(scalar_all(k)),
        _ => None,
    }
}

/// Helper for known CPUs where every kernel is pinned to scalar. Keeps the
/// per-kernel dimension explicit at the call site.
fn scalar_all(_k: Kernel) -> Isa {
    Isa::Scalar
}

/// Detect the host and look up its authoritative kernel choice for `k`, or
/// `None` when the host is unknown / has no accessible CPUID. Each primitive's
/// `choose()` calls this before calibrating: on a hit it picks the matching
/// candidate with no timing.
pub(crate) fn table_isa(k: Kernel) -> Option<Isa> {
    detect().and_then(|cpu| table_pick(cpu, k))
}

/// Pick the `(Isa, kernel)` pair for the host's authoritative table choice for
/// `k`, but only if that ISA survived the caller's capped candidate build.
/// Returns `None` on a table miss or when the table ISA isn't a candidate, so
/// the caller falls through to calibration. Shared by all primitives' `choose()`.
pub(crate) fn table_pick_pair<F: Copy>(cands: &[(Isa, F)], k: Kernel) -> Option<(Isa, F)> {
    let isa = table_isa(k)?;
    cands.iter().copied().find(|(i, _)| *i == isa)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_does_not_panic() {
        // Values vary by host/CI runner; only assert it doesn't panic and is
        // stable across calls (memoised).
        let a = detect();
        let b = detect();
        assert_eq!(a, b);
    }

    #[test]
    fn decode_intel_sapphire_rapids_family6_model143() {
        // Sapphire Rapids: display family 6, display model 143 (0x8F).
        // base_family = 6, base_model = 0xF, ext_model = 0x8 → model = 0x8F = 143.
        // EAX layout: ext_model in bits 19:16, base_family in 11:8, base_model 7:4.
        let eax = (0x8 << 16) | (0x6 << 8) | (0xF << 4);
        assert_eq!(decode_family_model(eax), (6, 143));
    }

    #[test]
    fn decode_amd_zen4_family25_model97() {
        // Zen4: display family 25 (0x19), display model 97 (0x61).
        // base_family = 0xF, ext_family = 25 - 15 = 10 (0xA).
        // base_model = 0x1, ext_model = 0x6 → model = 0x61 = 97.
        let eax = (0x6 << 16) | (0xA << 20) | (0xF << 8) | (0x1 << 4);
        assert_eq!(decode_family_model(eax), (25, 97));
    }

    #[test]
    fn decode_family_model_no_extension_when_base_below_6() {
        // base_family = 5 (< 6 and != 0xF): no extended model applied even if the
        // ext_model bits are set; no extended family since base != 0xF.
        let eax = (0x7 << 16) | (0x5 << 8) | (0x3 << 4);
        assert_eq!(decode_family_model(eax), (5, 3));
    }

    #[test]
    fn decode_vendor_strings() {
        // "GenuineIntel" = EBX "Genu", EDX "ineI", ECX "ntel".
        let ebx = u32::from_le_bytes(*b"Genu");
        let edx = u32::from_le_bytes(*b"ineI");
        let ecx = u32::from_le_bytes(*b"ntel");
        assert_eq!(decode_vendor(ebx, edx, ecx), Vendor::Intel);

        // "AuthenticAMD" = EBX "Auth", EDX "enti", ECX "cAMD".
        let ebx = u32::from_le_bytes(*b"Auth");
        let edx = u32::from_le_bytes(*b"enti");
        let ecx = u32::from_le_bytes(*b"cAMD");
        assert_eq!(decode_vendor(ebx, edx, ecx), Vendor::Amd);

        assert_eq!(decode_vendor(0, 0, 0), Vendor::Other);
    }

    #[test]
    fn table_pins_known_cpus_to_scalar_all_kernels() {
        let kernels = [
            Kernel::Intersect,
            Kernel::LowerBound,
            Kernel::Merge,
            Kernel::Dedup,
            Kernel::FilterRange,
            Kernel::FilterIndicesEq,
            Kernel::Gather,
        ];
        let zen4 = CpuKey {
            vendor: Vendor::Amd,
            family: 25,
            model: 97,
        };
        let spr = CpuKey {
            vendor: Vendor::Intel,
            family: 6,
            model: 143,
        };
        for k in kernels {
            assert_eq!(table_pick(zen4, k), Some(Isa::Scalar), "{k:?}");
            assert_eq!(table_pick(spr, k), Some(Isa::Scalar), "{k:?}");
        }
    }

    #[test]
    fn table_misses_unknown_cpu() {
        let unknown = CpuKey {
            vendor: Vendor::Other,
            family: 6,
            model: 143,
        };
        assert_eq!(table_pick(unknown, Kernel::Intersect), None);
        // Known vendor/family but unknown model.
        let other_intel = CpuKey {
            vendor: Vendor::Intel,
            family: 6,
            model: 42,
        };
        assert_eq!(table_pick(other_intel, Kernel::Intersect), None);
    }

    #[test]
    fn kernel_names_are_stable() {
        assert_eq!(Kernel::Intersect.name(), "intersect");
        assert_eq!(Kernel::LowerBound.name(), "lower_bound");
        assert_eq!(Kernel::Merge.name(), "merge");
        assert_eq!(Kernel::Dedup.name(), "dedup");
        assert_eq!(Kernel::FilterRange.name(), "filter_range");
        assert_eq!(Kernel::FilterIndicesEq.name(), "filter_indices_eq");
        assert_eq!(Kernel::Gather.name(), "gather");
    }
}
