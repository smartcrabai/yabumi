//! rand namespace (a homegrown PRNG, STDLIB.md §10, ARCHITECTURE.md §1.6/§2.1). effect: `rand`.
//! Cryptographic security is not required (SPEC §11.2; the crypto namespace is out of scope).
//! Mixes `std::time::SystemTime::now()`, `std::process::id()`, and the address of a local
//! variable on the stack (which varies across runs due to ASLR) as a seed, and implements a
//! small xoshiro256**-family PRNG.

use crate::eval::value::Value;
use crate::stdlib::{none_value, some_value};
use std::cell::RefCell;

/// xoshiro256**'s internal state.
struct Prng {
    state: [u64; 4],
}

impl Prng {
    /// Generates xoshiro256**'s initial state (4 words) via splitmix64, from a seed value that
    /// mixes `SystemTime::now()` (seconds + nanoseconds), `process::id()`, and the address of a
    /// local variable on the stack (which varies across runs due to ASLR) -- since an all-zero
    /// state is invalid for xoshiro256**, expanding through splitmix64 is the officially
    /// recommended initialization procedure.
    fn seed_from_entropy() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs();
        let subsec_nanos = u64::from(now.subsec_nanos());
        let pid = u64::from(std::process::id());
        let stack_marker = 0_u8;
        let stack_addr = std::ptr::addr_of!(stack_marker) as usize;
        let stack_bits = u64::try_from(stack_addr).unwrap_or(0);

        let seed = secs
            .wrapping_mul(1_000_000_007)
            .wrapping_add(subsec_nanos)
            .wrapping_add(pid.rotate_left(32))
            .wrapping_add(stack_bits.rotate_left(17));

        let mut sm_state = seed;
        let mut next_splitmix = move || {
            sm_state = sm_state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = sm_state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let state = [
            next_splitmix(),
            next_splitmix(),
            next_splitmix(),
            next_splitmix(),
        ];
        Self { state }
    }

    /// One step of xoshiro256** (David Blackman & Sebastiano Vigna, public domain).
    fn next_u64(&mut self) -> u64 {
        let result = self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);

        let t = self.state[1] << 17;

        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];

        self.state[2] ^= t;
        self.state[3] = self.state[3].rotate_left(45);

        result
    }
}

thread_local! {
    // Each of `par`'s worker threads has its own independent PRNG state (initialized per
    // thread). Since SPEC doesn't require deterministic results (only requiring determinism in
    // degenerate ranges, SAMPLES_PLAN.md §1.4), there's no need to share state across threads.
    static PRNG: RefCell<Prng> = RefCell::new(Prng::seed_from_entropy());
}

fn with_prng<R>(f: impl FnOnce(&mut Prng) -> R) -> R {
    PRNG.with(|cell| f(&mut cell.borrow_mut()))
}

/// A uniform `u64` random number in `[0, bound)`. `bound == 0` is outside the caller's
/// responsibility (an empty range), so `0` is returned.
fn uniform_below(bound: u64) -> u64 {
    if bound == 0 {
        return 0;
    }
    with_prng(|p| p.next_u64() % bound)
}

/// `int(lo: int, hi: int): int uses {rand}`. The half-open interval `[lo, hi)`.
#[must_use]
pub fn int(lo: i64, hi: i64) -> Value {
    let width = i128::from(hi) - i128::from(lo);
    if width <= 0 {
        return Value::Int(lo);
    }
    let range = u64::try_from(width)
        .unwrap_or_else(|_| unreachable!("distance between i64 values fits u64"));
    let offset = uniform_below(range);
    let value = i128::from(lo) + i128::from(offset);
    Value::Int(i64::try_from(value).unwrap_or_else(|_| unreachable!("sampled value is below hi")))
}

/// `float(): float uses {rand}`. `[0.0, 1.0)`.
#[must_use]
pub fn float() -> Value {
    // Extracting the top 53 bits and dividing by 2^53 (the largest power of 2 that f64 can
    // represent exactly) always produces a uniform random number that's exactly representable
    // (since 53 bits matches f64's mantissa precision, this particular conversion incurs no
    // cast_precision_loss -- the divisor is written as a literal 2^53 directly, avoiding a
    // cast).
    const TWO_POW_53: f64 = 9_007_199_254_740_992.0;
    let top53 = with_prng(Prng::next_u64) >> 11;
    #[expect(
        clippy::cast_precision_loss,
        reason = "Converts a value already truncated to 53 bits into f64's mantissa (52 bits + \
                  implicit leading 1 bit), so no precision loss occurs in this particular value \
                  range (clippy can't track the value range itself)"
    )]
    let numerator = top53 as f64;
    Value::Float(numerator / TWO_POW_53)
}

/// `bool(): bool uses {rand}`.
#[must_use]
pub fn bool_() -> Value {
    Value::Bool(with_prng(Prng::next_u64) & 1 == 1)
}

/// `choice[T](xs: list[T]): Option[T] uses {rand}`. An empty list gives None (doesn't abort
/// immediately).
#[must_use]
pub fn choice(xs: &[Value]) -> Value {
    if xs.is_empty() {
        return none_value();
    }
    let len = xs.len() as u64;
    let idx = usize::try_from(uniform_below(len)).unwrap_or(0);
    some_value(xs[idx].clone())
}

/// `shuffle[T](self: var list[T]): void uses {rand}` (Fisher-Yates).
pub fn shuffle(xs: &mut std::sync::Arc<Vec<Value>>) {
    let vec = std::sync::Arc::make_mut(xs);
    let len = vec.len();
    if len < 2 {
        return;
    }
    for i in (1..len).rev() {
        let bound = u64::try_from(i + 1).unwrap_or(1);
        let j = usize::try_from(uniform_below(bound)).unwrap_or(0);
        vec.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_degenerate_half_open_interval_is_always_the_lower_bound() {
        for _ in 0..20 {
            assert_eq!(int(5, 6), Value::Int(5));
        }
    }

    #[test]
    fn int_inverted_range_falls_back_to_lo() {
        assert_eq!(int(10, 10), Value::Int(10));
        assert_eq!(int(10, 5), Value::Int(10));
    }

    #[test]
    fn int_stays_within_half_open_bounds() {
        for _ in 0..200 {
            let Value::Int(n) = int(0, 10) else {
                panic!("expected int")
            };
            assert!((0..10).contains(&n));
        }
    }

    #[test]
    fn float_stays_within_zero_one_half_open_interval() {
        for _ in 0..200 {
            let Value::Float(x) = float() else {
                panic!("expected float")
            };
            assert!((0.0..1.0).contains(&x));
        }
    }

    #[test]
    fn bool_is_true_or_false() {
        let Value::Bool(_) = bool_() else {
            panic!("expected bool")
        };
    }

    #[test]
    fn choice_on_empty_list_is_none() {
        let result = choice(&[]);
        let Value::Enum(inst) = &result else {
            panic!("expected Option[T]")
        };
        assert_eq!(inst.variant_name.as_ref(), "None");
    }

    #[test]
    fn choice_on_single_element_list_always_returns_that_element() {
        let xs = [Value::Int(42)];
        for _ in 0..20 {
            let result = choice(&xs);
            let Value::Enum(inst) = &result else {
                panic!("expected Option[T]")
            };
            assert_eq!(inst.variant_name.as_ref(), "Some");
            assert_eq!(inst.fields[0], Value::Int(42));
        }
    }

    #[test]
    fn shuffle_on_single_element_list_cannot_change_order() {
        let mut xs = std::sync::Arc::new(vec![Value::Int(42)]);
        shuffle(&mut xs);
        assert_eq!(*xs, vec![Value::Int(42)]);
    }

    #[test]
    fn shuffle_preserves_the_multiset_of_elements() {
        let original = vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ];
        let mut xs = std::sync::Arc::new(original.clone());
        shuffle(&mut xs);
        let mut after = (*xs).clone();
        let mut before = original;
        after.sort_by_key(|v| {
            let Value::Int(n) = v else { unreachable!() };
            *n
        });
        before.sort_by_key(|v| {
            let Value::Int(n) = v else { unreachable!() };
            *n
        });
        assert_eq!(after, before);
    }

    #[test]
    fn shuffle_on_empty_list_does_not_panic() {
        let mut xs: std::sync::Arc<Vec<Value>> = std::sync::Arc::new(vec![]);
        shuffle(&mut xs);
        assert!(xs.is_empty());
    }

    /// Verifies SPEC §11.2 / STDLIB.md §10 through the full pipeline
    /// (`samples/ok/11-2_rand/entry_main.ybm`).
    #[test]
    fn sample_rand_runs_end_to_end() {
        let result = crate::stdlib::builtins::test_pipeline::run_ok_sample("11-2_rand");
        assert!(
            result.is_ok(),
            "sample should run without Abort: {result:?}"
        );
    }
}
