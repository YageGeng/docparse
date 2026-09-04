// SPDX-License-Identifier: Apache-2.0
// Derived from LiteParse revision b2e76ec5b0c1cb4eb11d67296e916792f4fb5858 and modified for docparse.
#[derive(Debug, Clone, Copy, Default)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CharBox {
    pub left: f64,
    pub right: f64,
    pub bottom: f64,
    pub top: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TextRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}
