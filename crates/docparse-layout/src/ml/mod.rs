//! Shared machine-learning support for layout inference.

pub(crate) mod backend;
pub(crate) mod imageproc;

/// Computes a scalar sigmoid for post-processing logits.
pub(crate) fn sigmoid_f32(value: f32) -> f32 {
    1.0 / (1.0 + (-value).exp())
}
