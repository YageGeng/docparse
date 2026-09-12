//! CTC decoding keeps compact argmax results instead of copying a full vocabulary tensor.
use crate::OcrError;
use ndarray::ArrayView3;

/// Compact probability maxima with one entry per model timestep.
pub(crate) struct CtcSteps {
    pub classes: usize,
    pub indices: Vec<usize>,
    pub scores: Vec<f32>,
}

impl TryFrom<ArrayView3<'_, f32>> for CtcSteps {
    type Error = OcrError;

    /// Validates the tensor and extracts deterministic first-index maxima before releasing the session.
    fn try_from(values: ArrayView3<'_, f32>) -> Result<Self, Self::Error> {
        let (batch, time, classes) = values.dim();
        if batch != 1
            || !(1..=8192).contains(&time)
            || !(2..=65536).contains(&classes)
        {
            return Err(OcrError::InvalidData(
                "expected bounded [1,T,C] recognition probabilities".to_owned(),
            ));
        }
        let mut indices = Vec::with_capacity(time);
        let mut scores = Vec::with_capacity(time);
        for row in values.index_axis(ndarray::Axis(0), 0).outer_iter() {
            let mut best = (0, f32::NEG_INFINITY);
            for (index, &score) in row.iter().enumerate() {
                if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                    return Err(OcrError::InvalidData(
                        "non-finite or out-of-range CTC probability".to_owned(),
                    ));
                }
                // Strict comparison preserves the first class on ties, matching Paddle's argmax.
                if score > best.1 {
                    best = (index, score);
                }
            }
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
                CtcSteps::try_from(probabilities.view()).expect("CTC tensor"),
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
