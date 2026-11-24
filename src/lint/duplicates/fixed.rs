//! The integer arithmetic of the score: a natural logarithm in fixed point
//! and a 256-bit product for the exact threshold test. A float's `ln` comes
//! from the platform's libm, which need not round it the same on two
//! machines; this one gives the same integers everywhere.

/// One in Q64, the fixed point of [`ln`].
const ONE: u128 = 1 << 64;

/// ln 2 in Q64, rounded to nearest: 0.693147180559945309417... times 2^64.
const LN2: u128 = 0xB172_17F7_D1CF_79AC;

/// The natural logarithm of `x` in Q64 (units of 2^-64), fewer than 90
/// units from the exact value; 0 for `x` of 0 or 1.
///
/// `x = m 2^k` with `m` in [1, 2), and ln x = k ln 2 + 2 atanh(z). Here
/// z = (m - 1) / (m + 1) is below 1/3, and atanh(z) = z + z^3/3 + z^5/5 +
/// and so on. Each term is under a ninth of the one before, so at most 21
/// are above zero in Q64. Every cut rounds down: z by under 1 unit, z^2 by
/// under 2. Each term is cut by under 1.9: its predecessor's error over 9,
/// the cut of z^2 times a term under a third, and its own cut. So the
/// series loses under 27 units with its divisions and its tail, 54
/// doubled. `LN2` is off by
/// at most half a unit, k <= 63 times: under 32.
pub(super) fn ln(x: u64) -> u128 {
    let Some(k) = x.checked_ilog2() else {
        return 0;
    };
    // x < 2^(k+1) and k <= 63: m is below 2^65.
    let m = u128::from(x) << (64 - k);
    // m - ONE < 2^64, so the shift stays below 2^128.
    let z = ((m - ONE) << 64) / (m + ONE);
    // z < 2^64 / 3, so z * z < 2^128 / 9.
    let z2 = (z * z) >> 64;
    let (mut term, mut sum, mut odd) = (z, 0u128, 1u128);
    while term > 0 {
        sum += term / odd;
        // term <= z and z2 < 2^61: the product stays below 2^124.
        term = (term * z2) >> 64;
        odd += 2;
    }
    u128::from(k) * LN2 + 2 * sum
}

/// `a * b` as 256 bits, (high, low): compared as a tuple, the order of the
/// products.
pub(super) fn wide(a: u128, b: u128) -> (u128, u128) {
    const LOW: u128 = (1 << 64) - 1;
    let (a1, a0) = (a >> 64, a & LOW);
    let (b1, b0) = (b >> 64, b & LOW);
    // Each partial product of two 64-bit halves stays below 2^128.
    let (low, cross, high) = (a0 * b0, a0 * b1, a1 * b1);
    let (middle, middle_carry) = cross.overflowing_add(a1 * b0);
    let (low, low_carry) = low.overflowing_add(middle << 64);
    // The product is below 2^256, so the high half takes every carry.
    let high = high + (middle >> 64) + (u128::from(middle_carry) << 64) + u128::from(low_carry);
    (high, low)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each `x` with floor(ln(x) 2^64) from 100-digit decimal arithmetic,
    /// and the bits `ln` gives: pinned, since a change of the series or of
    /// its cuts moves every score.
    const LOGARITHMS: [(u64, u128, u128); 8] = [
        (2, 12_786_308_645_202_655_659, 12_786_308_645_202_655_660),
        (3, 20_265_819_725_292_939_638, 20_265_819_725_292_939_628),
        (10, 42_475_197_918_399_869_019, 42_475_197_918_399_869_010),
        (
            764,
            122_459_961_022_726_631_992,
            122_459_961_022_726_631_982,
        ),
        (
            1000,
            127_425_593_755_199_607_058,
            127_425_593_755_199_607_042,
        ),
        (
            1 << 32,
            409_161_876_646_484_981_113,
            409_161_876_646_484_981_120,
        ),
        (
            1 << 63,
            805_537_444_647_767_306_566,
            805_537_444_647_767_306_580,
        ),
        (
            u64::MAX,
            818_323_753_292_969_962_225,
            818_323_753_292_969_962_224,
        ),
    ];

    #[test]
    fn ln_gives_pinned_bits_within_its_bound_of_the_exact_value() {
        assert_eq!((ln(0), ln(1), ln(2)), (0, 0, LN2));
        let computed: Vec<u128> = LOGARITHMS.iter().map(|&(x, _, _)| ln(x)).collect();
        let pinned: Vec<u128> = LOGARITHMS.iter().map(|&(_, _, bits)| bits).collect();
        assert_eq!(computed, pinned);
        for (x, exact, bits) in LOGARITHMS {
            // The exact value is within one unit above its floor.
            let error = bits.abs_diff(exact) + 1;
            assert!(error < 90, "ln({x}) is {error} units off");
        }
    }

    #[test]
    fn ln_grows_with_its_argument() {
        let mut last = 0;
        for x in (1..5000).chain([u64::MAX - 1, u64::MAX]) {
            let now = ln(x);
            assert!(now >= last, "ln({x})");
            last = now;
        }
    }

    #[test]
    fn wide_is_the_full_product() {
        assert_eq!(wide(0, u128::MAX), (0, 0));
        assert_eq!(wide(1 << 64, 1 << 64), (1, 0));
        assert_eq!(wide(u128::MAX, u128::MAX), (u128::MAX - 1, 1));
        assert_eq!(wide(u128::MAX, 2), (1, u128::MAX - 1));
        let (a, b) = (0x1234_5678_9abc_def0_u128 << 60, 0xfedc_ba98_u128 << 70);
        let shifted = (a >> 60) * (b >> 70);
        // a * b = shifted * 2^130: the high half holds it from bit 2.
        assert_eq!(wide(a, b), (shifted << 2, 0));
    }
}
