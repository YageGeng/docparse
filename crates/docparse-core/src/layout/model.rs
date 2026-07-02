//! Backend-neutral model input and output containers.

/// PP-StructureV3 model input tensors.
pub struct LayoutInput {
    /// Image tensor in NCHW format.
    pub image: ndarray::Array4<f32>,
    /// Model image shape input.
    pub im_shape: ndarray::Array2<f32>,
    /// Resize scale factor input.
    pub scale_factor: ndarray::Array2<f32>,
    /// Original image width in pixels.
    pub original_width: u32,
    /// Original image height in pixels.
    pub original_height: u32,
}

/// PP-StructureV3 model input tensors for multiple images.
pub struct LayoutBatchInput {
    /// Image tensor in batched NCHW format.
    pub image: ndarray::Array4<f32>,
    /// Model image shape input with one row per image.
    pub im_shape: ndarray::Array2<f32>,
    /// Resize scale factor input with one row per image.
    pub scale_factor: ndarray::Array2<f32>,
    /// Original image sizes in batch order.
    pub original_sizes: Vec<OriginalImageSize>,
}

/// Original image dimensions retained so batched outputs can be split per page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OriginalImageSize {
    /// Original image width in pixels.
    pub width: u32,
    /// Original image height in pixels.
    pub height: u32,
}

/// Backend-neutral tensor view copied from ORT outputs.
#[derive(Debug, Clone)]
pub struct LayoutTensor {
    /// Tensor name when known.
    pub name: Option<String>,
    /// Tensor shape.
    pub shape: Vec<usize>,
    /// Tensor values in row-major order.
    pub values: Vec<f32>,
}

/// Backend-neutral model output collection.
#[derive(Debug, Clone)]
pub struct ModelOutput {
    /// Output tensors.
    pub tensors: Vec<LayoutTensor>,
}
