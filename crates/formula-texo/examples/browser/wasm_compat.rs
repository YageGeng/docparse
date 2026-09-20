//! A browser Worker can own this model directly without the PDF/WebUI stack.
#[cfg(target_arch = "wasm32")]
mod browser {
    use docparse_common::timing::Timings;
    use docparse_config::{RawConfig, ValidatedConfig};
    use docparse_formula::FormulaEngine;
    use docparse_formula_texo::{TexoArtifacts, TexoEngine};
    use docparse_layout::{PageImage, PageImageInput, PixelFormat};
    use std::sync::Arc;
    use wasm_bindgen::prelude::*;

    /// Owns the asynchronous browser model independently of any particular image request.
    #[wasm_bindgen]
    pub struct BrowserTexo {
        engine: TexoEngine,
    }

    #[wasm_bindgen]
    impl BrowserTexo {
        /// Initializes the self-hosted runtime and loads explicitly supplied model bytes once.
        #[wasm_bindgen(js_name = "load")]
        pub async fn load(
            encoder: Vec<u8>,
            decoder: Vec<u8>,
            tokenizer: Vec<u8>,
            runtime_url: String,
            webgpu: bool,
        ) -> Result<BrowserTexo, JsValue> {
            let dist =
                ort_web::Dist::new(runtime_url).with_script_name(if webgpu {
                    "ort.webgpu.min.js"
                } else {
                    "ort.wasm.min.js"
                });
            ort::set_api(
                ort_web::api(dist)
                    .await
                    .map_err(|error| JsValue::from_str(&error.to_string()))?,
            );
            let mut raw = RawConfig::default();
            raw.formula.engine =
                vec![docparse_config::FormulaEngineConfig::Texo(
                    docparse_config::TexoFormulaConfig::default(),
                )];
            raw.render.workers = 1;
            raw.render.queue_size = 2;
            let config = ValidatedConfig::try_from(raw)
                .map_err(|error| JsValue::from_str(&error.to_string()))?
                .with_webgpu(webgpu);
            let engine = TexoEngine::from_artifacts(
                Arc::new(config),
                TexoArtifacts {
                    encoder: Arc::from(encoder),
                    decoder: Arc::from(decoder),
                    tokenizer: Arc::from(tokenizer),
                },
            )
            .await
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
            Ok(BrowserTexo { engine })
        }

        /// Recognizes a decoded PNG, optionally repeating it within one actual batch.
        pub async fn recognize(
            &self,
            png: Vec<u8>,
            batch_size: usize,
        ) -> Result<String, JsValue> {
            if !(1..=32).contains(&batch_size) {
                return Err(JsValue::from_str("batch size must be 1..32"));
            }
            let rgb = image::load_from_memory(&png)
                .map_err(|error| JsValue::from_str(&error.to_string()))?
                .to_rgb8();
            let page = Arc::new(
                PageImage::try_from(
                    PageImageInput::builder()
                        .width(rgb.width())
                        .height(rgb.height())
                        .pixel_format(PixelFormat::Rgb8)
                        .data(Arc::from(rgb.into_raw()))
                        .build(),
                )
                .map_err(|error| JsValue::from_str(&error.to_string()))?,
            );
            let result = self
                .engine
                .recognize(vec![page; batch_size], Timings::default())
                .await
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            serde_json::to_string(&result)
                .map_err(|error| JsValue::from_str(&error.to_string()))
        }
    }
}
