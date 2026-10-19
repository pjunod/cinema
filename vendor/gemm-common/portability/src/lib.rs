//! Executable regression for generic ARM codegen and runtime GEMM dispatch.

#[cfg(all(test, target_arch = "aarch64"))]
mod tests {
    use half::f16;

    #[test]
    fn half_precision_matrix_dispatch_respects_detected_cpu_features() {
        let fp16 = std::arch::is_aarch64_feature_detected!("fp16");
        match std::env::var("PLURX_FP16_EXPECTED").as_deref() {
            Ok("present") => assert!(fp16, "native FP16 path must be available"),
            Ok("absent") => assert!(!fp16, "fallback proof requires CPU without FP16"),
            _ => panic!("set PLURX_FP16_EXPECTED to present or absent"),
        }
        println!("runtime fp16={fp16}; validating detected GEMM dispatch");
        for (m, n, k) in [(17, 19, 23), (64, 64, 64), (1, 19, 23)] {
            let lhs: Vec<_> = (0..m * k)
                .map(|i| f16::from_f32((i % 7) as f32 - 3.0))
                .collect();
            let rhs: Vec<_> = (0..k * n)
                .map(|i| f16::from_f32((i % 5) as f32 - 2.0))
                .collect();
            let mut actual = vec![f16::from_f32(-2.0); m * n];
            // All matrices are column-major, with valid lengths and disjoint buffers.
            unsafe {
                gemm::gemm(
                    m,
                    n,
                    k,
                    actual.as_mut_ptr(),
                    m as isize,
                    1,
                    true,
                    lhs.as_ptr(),
                    m as isize,
                    1,
                    rhs.as_ptr(),
                    k as isize,
                    1,
                    f16::from_f32(1.0),
                    f16::from_f32(2.0),
                    false,
                    false,
                    false,
                    gemm::Parallelism::None,
                );
            }
            for col in 0..n {
                for row in 0..m {
                    let dot: f32 = (0..k)
                        .map(|inner| lhs[row + inner * m].to_f32() * rhs[inner + col * k].to_f32())
                        .sum();
                    assert_eq!(
                        actual[row + col * m].to_f32(),
                        -2.0 + 2.0 * dot,
                        "shape {m}x{n}x{k} at {row},{col}; fp16={fp16}"
                    );
                }
            }
        }
    }

    #[test]
    fn native_fp16_vector_helpers_execute_only_when_detected() {
        if !std::arch::is_aarch64_feature_detected!("fp16") {
            return;
        }
        use gemm_common::simd::aarch64::{vaddq_f16, vfmaq_f16, vfmaq_laneq_f16, vmulq_f16};
        let a = [f16::from_f32(2.0).to_bits(); 8];
        let b: [u16; 8] = std::array::from_fn(|i| f16::from_f32(i as f32 + 1.0).to_bits());
        let c = [f16::from_f32(3.0).to_bits(); 8];
        // Runtime feature detection above guards every FP16 instruction call.
        unsafe {
            let a = std::mem::transmute::<[u16; 8], core::arch::aarch64::uint16x8_t>(a);
            let b = std::mem::transmute::<[u16; 8], core::arch::aarch64::uint16x8_t>(b);
            let c = std::mem::transmute::<[u16; 8], core::arch::aarch64::uint16x8_t>(c);
            for (values, expected) in [
                (vmulq_f16(a, b), 0),
                (vaddq_f16(a, b), 1),
                (vfmaq_f16(c, a, b), 2),
            ] {
                let values: [u16; 8] = std::mem::transmute(values);
                for (i, value) in values.into_iter().enumerate() {
                    let b = i as f32 + 1.0;
                    let expected = match expected {
                        0 => 2.0 * b,
                        1 => 2.0 + b,
                        _ => 3.0 + 2.0 * b,
                    };
                    assert_eq!(f16::from_bits(value).to_f32(), expected);
                }
            }
            macro_rules! lane {
                ($n:literal) => {{
                    let values: [u16; 8] = std::mem::transmute(vfmaq_laneq_f16::<$n>(c, a, b));
                    assert!(values
                        .into_iter()
                        .all(|v| f16::from_bits(v).to_f32() == 3.0 + 2.0 * ($n as f32 + 1.0)));
                }};
            }
            lane!(0);
            lane!(1);
            lane!(2);
            lane!(3);
            lane!(4);
            lane!(5);
            lane!(6);
            lane!(7);
        }
    }
}
