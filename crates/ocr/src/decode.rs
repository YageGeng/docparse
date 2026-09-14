//! CTC decoding keeps compact argmax results instead of copying a full vocabulary tensor.
use crate::OcrError;
use ndarray::{ArrayView1, ArrayView2};

/// Compact probability maxima with one entry per model timestep.
pub(crate) struct CtcSteps {
    pub classes: usize,
    pub indices: Vec<usize>,
    pub scores: Vec<f32>,
}

impl CtcSteps {
    /// Uses independent lanes for vectorizable validation/maxima, then retains the first exact maximum.
    #[expect(
        clippy::float_cmp,
        reason = "The maximum is selected from the same input; exact ties must retain the first index."
    )]
    fn maximum(row: ArrayView1<'_, f32>) -> Result<(usize, f32), OcrError> {
        let invalid = || {
            OcrError::InvalidData(
                "non-finite or out-of-range CTC probability".to_owned(),
            )
        };
        let Some(values) = row.as_slice() else {
            // Public ndarray views may be strided; keep the same strict scalar contract for them.
            return row.iter().enumerate().try_fold(
                (0, f32::NEG_INFINITY),
                |best, (index, &score)| {
                    if !(0.0..=1.0).contains(&score) {
                        return Err(invalid());
                    }
                    Ok(if score > best.1 { (index, score) } else { best })
                },
            );
        };
        let (chunks, tail) = values.as_chunks::<8>();
        let mut maxima = [f32::NEG_INFINITY; 8];
        let mut valid = [true; 8];
        for chunk in chunks {
            for ((maximum, valid), &score) in
                maxima.iter_mut().zip(&mut valid).zip(chunk)
            {
                *valid &= (0.0..=1.0).contains(&score);
                *maximum = maximum.max(score);
            }
        }
        let mut maximum = maxima.into_iter().fold(f32::NEG_INFINITY, f32::max);
        let mut valid = valid.into_iter().all(|value| value);
        for &score in tail {
            valid &= (0.0..=1.0).contains(&score);
            maximum = maximum.max(score);
        }
        if !valid {
            return Err(invalid());
        }
        let index = values
            .iter()
            .position(|&score| score == maximum)
            .ok_or_else(invalid)?;
        // Read the original value so even a tied negative zero retains its original bit pattern.
        Ok((index, *values.get(index).ok_or_else(invalid)?))
    }
}

impl TryFrom<ArrayView2<'_, f32>> for CtcSteps {
    type Error = OcrError;

    /// Reduces one batch member independently, retaining first-index ties and strict probability validation.
    fn try_from(values: ArrayView2<'_, f32>) -> Result<Self, Self::Error> {
        let (time, classes) = values.dim();
        if !(1..=8192).contains(&time) || !(2..=65536).contains(&classes) {
            return Err(OcrError::InvalidData(
                "expected bounded [T,C] recognition probabilities".to_owned(),
            ));
        }
        let mut indices = Vec::with_capacity(time);
        let mut scores = Vec::with_capacity(time);
        for row in values.outer_iter() {
            let best = Self::maximum(row)?;
            indices.push(best.0);
            scores.push(best.1);
        }
        Ok(Self {
            classes,
            indices,
            scores,
        })
    }
}

/// Vocabulary entries are strings so multi-codepoint characters are never truncated.
pub(crate) struct Dictionary(pub Vec<String>);

impl Dictionary {
    /// Collapses repeated CTC classes and blanks, preserving repeated letters separated by blank.
    pub fn decode(&self, steps: CtcSteps) -> Result<(String, f64), OcrError> {
        if steps.classes != self.0.len()
            || steps.indices.len() != steps.scores.len()
            || steps.indices.is_empty()
        {
            return Err(OcrError::InvalidData(
                "CTC vocabulary or sequence length mismatch".to_owned(),
            ));
        }
        let mut text = String::new();
        let mut previous = 0;
        let (mut total, mut count) = (0.0, 0);
        for (class, score) in steps.indices.into_iter().zip(steps.scores) {
            let token = self.0.get(class).ok_or_else(|| {
                OcrError::InvalidData("CTC class outside dictionary".to_owned())
            })?;
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                return Err(OcrError::InvalidData(
                    "invalid CTC confidence".to_owned(),
                ));
            }
            if class != 0 && class != previous {
                text.push_str(token);
                total += f64::from(score);
                count += 1;
            }
            previous = class;
        }
        Ok((
            text,
            if count == 0 {
                0.0
            } else {
                total / f64::from(count)
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndarray::Array3;

    /// Lanes, tails and strided views must preserve ties and reject invalid values anywhere in a row.
    #[test]
    fn probability_maximum_preserves_scalar_contract() {
        for width in [2, 7, 8, 9, 17, 18710] {
            // Unique maxima must recover their position across vector lanes and the scalar tail.
            for winner in [0, width / 2, width - 1] {
                let mut row = ndarray::Array1::from_elem(width, 0.25_f32);
                *row.get_mut(winner).expect("winner") = 0.75;
                assert_eq!(
                    CtcSteps::maximum(row.view()).expect("maximum"),
                    (winner, 0.75)
                );
            }
            let mut values = ndarray::Array2::zeros((width, 2));
            *values.get_mut((0, 0)).expect("first") = 1.0;
            *values.get_mut((width - 1, 0)).expect("last") = 1.0;
            let strided = values.column(0);
            let contiguous = strided.to_owned();
            assert_eq!(CtcSteps::maximum(strided).expect("strided"), (0, 1.0));
            assert_eq!(
                CtcSteps::maximum(contiguous.view()).expect("contiguous"),
                (0, 1.0)
            );
            for invalid in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
                let mut row = contiguous.clone();
                *row.last_mut().expect("tail") = invalid;
                assert!(matches!(
                    CtcSteps::maximum(row.view()),
                    Err(OcrError::InvalidData(_))
                ));
            }
        }
        let zeros = ndarray::arr1(&[-0.0_f32, 0.0]);
        assert_eq!(
            CtcSteps::maximum(zeros.view()).expect("zero").1.to_bits(),
            (-0.0_f32).to_bits()
        );
    }

    /// Compares the original scalar scan with the optimized reduction on the pinned vocabulary width.
    #[test]
    #[ignore = "manual release CPU throughput probe"]
    fn probability_reduction_throughput() {
        use std::{hint::black_box, time::Instant};
        let values = ndarray::Array2::from_shape_fn((100, 18710), |(t, c)| {
            ((t * 7 + c * 13) % 1000) as f32 / 1000.0
        });
        let started = Instant::now();
        for _ in 0..50 {
            for row in black_box(&values).outer_iter() {
                let mut best = (0, f32::NEG_INFINITY);
                for (index, &score) in row.iter().enumerate() {
                    assert!(score.is_finite() && (0.0..=1.0).contains(&score));
                    if score > best.1 {
                        best = (index, score);
                    }
                }
                black_box(best);
            }
        }
        let scalar = started.elapsed();
        let started = Instant::now();
        for _ in 0..50 {
            for row in black_box(&values).outer_iter() {
                black_box(CtcSteps::maximum(row).expect("valid"));
            }
        }
        println!(
            "scalar_ms={} lanes_ms={}",
            scalar.as_secs_f64() * 1000.0,
            started.elapsed().as_secs_f64() * 1000.0
        );
    }

    /// Batch boundaries must reset CTC repetition state and retain strict validation for every sample.
    #[test]
    fn ctc_batch_members_decode_independently() {
        let dictionary = Dictionary(vec!["".into(), "a".into(), "b".into()]);
        let mut probabilities = Array3::zeros((2, 3, 3));
        for (batch, indices) in [[1, 0, 1], [2, 2, 1]].into_iter().enumerate() {
            for (time, class) in indices.into_iter().enumerate() {
                *probabilities
                    .get_mut((batch, time, class))
                    .expect("probability") = 0.9;
            }
        }
        let texts: Vec<_> = probabilities
            .outer_iter()
            .map(|sample| {
                dictionary
                    .decode(CtcSteps::try_from(sample).expect("sample"))
                    .expect("text")
                    .0
            })
            .collect();
        assert_eq!(texts, ["aa", "ba"]);
        *probabilities.get_mut((1, 0, 1)).expect("probability") = f32::NAN;
        assert!(matches!(
            CtcSteps::try_from(probabilities.index_axis(ndarray::Axis(0), 1)),
            Err(OcrError::InvalidData(_))
        ));
    }

    /// Blank separates legitimate repeated letters; Unicode vocabulary entries and space retain their full text.
    #[test]
    fn ctc_collapses_only_adjacent_nonblank_repeats() {
        let dictionary = Dictionary(vec![
            "".into(),
            "l".into(),
            "a".into(),
            "中".into(),
            "👩‍💻".into(),
            " ".into(),
        ]);
        let mut probabilities = Array3::zeros((1, 10, 6));
        for (time, class) in
            [1, 1, 0, 1, 2, 2, 5, 3, 4, 0].into_iter().enumerate()
        {
            *probabilities
                .get_mut((0, time, class))
                .expect("probability") = 0.9;
        }
        let result = dictionary
            .decode(
                CtcSteps::try_from(
                    probabilities.index_axis(ndarray::Axis(0), 0),
                )
                .expect("CTC tensor"),
            )
            .expect("decoded text");
        assert_eq!(result.0, "lla 中👩‍💻");
        assert!((result.1 - 0.9).abs() < 1e-6);
        dictionary
            .decode(CtcSteps {
                classes: 2,
                indices: vec![1],
                scores: vec![0.9],
            })
            .expect_err("mismatched vocabulary must fail");
    }
}
