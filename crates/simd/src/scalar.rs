//! Scalar oracle kernels. Always compiled; the reference for every SIMD
//! differential proptest (SPEC-12 NF3) and the fallback path on any ISA
//! without a matching kernel.

pub fn lower_bound(haystack: &[u64], value: u64) -> usize {
    haystack.partition_point(|&x| x < value)
}

pub fn intersect(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
}

pub fn merge(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] <= b[j] {
            out.push(a[i]);
            i += 1;
        } else {
            out.push(b[j]);
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
}

pub fn dedup(sorted: &[u64], out: &mut Vec<u64>) {
    let mut last: Option<u64> = None;
    for &v in sorted {
        if last != Some(v) {
            out.push(v);
            last = Some(v);
        }
    }
}

pub fn filter(values: &[u64], keep: impl Fn(u64) -> bool, out: &mut Vec<u64>) {
    for &v in values {
        if keep(v) {
            out.push(v);
        }
    }
}

pub fn gather(base: &[u64], indices: &[u32], out: &mut Vec<u64>) {
    for &i in indices {
        debug_assert!((i as usize) < base.len(), "gather index out of bounds");
        out.push(base[i as usize]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_bound_basic() {
        let h = [1u64, 3, 3, 5, 9];
        assert_eq!(lower_bound(&h, 3), 1);
        assert_eq!(lower_bound(&h, 4), 3);
        assert_eq!(lower_bound(&h, 0), 0);
        assert_eq!(lower_bound(&h, 10), 5);
    }

    #[test]
    fn intersect_basic() {
        let mut out = Vec::new();
        intersect(&[1, 2, 3, 5, 8], &[2, 3, 4, 8, 9], &mut out);
        assert_eq!(out, vec![2, 3, 8]);
    }

    #[test]
    fn merge_keeps_duplicates() {
        let mut out = Vec::new();
        merge(&[1, 3, 3, 5], &[2, 3, 6], &mut out);
        assert_eq!(out, vec![1, 2, 3, 3, 3, 5, 6]);
    }

    #[test]
    fn dedup_basic() {
        let mut out = Vec::new();
        dedup(&[1, 1, 2, 2, 2, 5], &mut out);
        assert_eq!(out, vec![1, 2, 5]);
    }

    #[test]
    fn filter_basic() {
        let mut out = Vec::new();
        filter(&[1, 2, 3, 4, 5], |v| v % 2 == 0, &mut out);
        assert_eq!(out, vec![2, 4]);
    }

    #[test]
    fn gather_basic() {
        let mut out = Vec::new();
        gather(&[10, 20, 30, 40], &[3, 0, 2], &mut out);
        assert_eq!(out, vec![40, 10, 30]);
    }
}
