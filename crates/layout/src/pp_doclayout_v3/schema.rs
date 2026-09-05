use std::collections::BTreeMap;
use std::path::Path;

use ort::session::Session;
use ort::value::{Outlet, ValueType};
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::LayoutError;

/// A fixed or symbolic ONNX tensor dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaDimension {
    Fixed(i64),
    Dynamic(String),
}

/// Stable neutral schema for one model input or output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TensorSchema {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<SchemaDimension>,
}

/// Stable model metadata that excludes paths and runtime-generated values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
pub struct ModelMetadataSchema {
    #[builder(default)]
    pub name: Option<String>,
    #[builder(default)]
    pub producer: Option<String>,
    #[builder(default)]
    pub domain: Option<String>,
    #[builder(default)]
    pub description: Option<String>,
    #[builder(default)]
    pub graph_description: Option<String>,
    #[builder(default)]
    pub version: Option<i64>,
    pub custom: BTreeMap<String, String>,
}

/// Complete deterministic input, output, and metadata schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSchema {
    pub inputs: Vec<TensorSchema>,
    pub outputs: Vec<TensorSchema>,
    pub metadata: ModelMetadataSchema,
}

impl ModelSchema {
    /// Rejects any input/output name, type, rank, or dimension outside the fixed contract.
    pub fn validate_pp_doclayout_v3(&self) -> Result<(), LayoutError> {
        let expected_inputs = vec![
            TensorSchema {
                name: "im_shape".to_owned(),
                dtype: "f32".to_owned(),
                shape: vec![
                    SchemaDimension::Dynamic("DynamicDimension.0".to_owned()),
                    SchemaDimension::Fixed(2),
                ],
            },
            TensorSchema {
                name: "image".to_owned(),
                dtype: "f32".to_owned(),
                shape: vec![
                    SchemaDimension::Dynamic("DynamicDimension.1".to_owned()),
                    SchemaDimension::Fixed(3),
                    SchemaDimension::Fixed(800),
                    SchemaDimension::Fixed(800),
                ],
            },
            TensorSchema {
                name: "scale_factor".to_owned(),
                dtype: "f32".to_owned(),
                shape: vec![
                    SchemaDimension::Dynamic("DynamicDimension.2".to_owned()),
                    SchemaDimension::Fixed(2),
                ],
            },
        ];
        let expected_outputs = vec![
            TensorSchema {
                name: "fetch_name_0".to_owned(),
                dtype: "f32".to_owned(),
                shape: vec![
                    SchemaDimension::Dynamic("DynamicDimension.3".to_owned()),
                    SchemaDimension::Fixed(7),
                ],
            },
            TensorSchema {
                name: "fetch_name_1".to_owned(),
                dtype: "i32".to_owned(),
                shape: vec![SchemaDimension::Dynamic(
                    "DynamicDimension.4".to_owned(),
                )],
            },
            TensorSchema {
                name: "fetch_name_2".to_owned(),
                dtype: "i32".to_owned(),
                shape: vec![
                    SchemaDimension::Dynamic("DynamicDimension.5".to_owned()),
                    SchemaDimension::Fixed(200),
                    SchemaDimension::Fixed(200),
                ],
            },
        ];
        if self.inputs != expected_inputs {
            return Err(LayoutError::UnsupportedModelSchema {
                reason: format!(
                    "input contract mismatch: expected {expected_inputs:?}, got {:?}",
                    self.inputs
                ),
            });
        }
        if self.outputs != expected_outputs {
            return Err(LayoutError::UnsupportedModelSchema {
                reason: format!(
                    "output contract mismatch: expected {expected_outputs:?}, got {:?}",
                    self.outputs
                ),
            });
        }
        Ok(())
    }

    /// Extracts a neutral schema from a loaded ORT session.
    pub(crate) fn from_session(session: &Session) -> Result<Self, LayoutError> {
        let inputs = session
            .inputs()
            .iter()
            .map(outlet_schema)
            .collect::<Result<_, _>>()?;
        let outputs = session
            .outputs()
            .iter()
            .map(outlet_schema)
            .collect::<Result<_, _>>()?;
        Ok(Self {
            inputs,
            outputs,
            metadata: metadata_schema(session)?,
        })
    }
}

/// Loads one ONNX file and returns only neutral schema information.
pub fn inspect_model(
    path: impl AsRef<Path>,
) -> Result<ModelSchema, LayoutError> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(LayoutError::ModelNotFound {
            path: path.to_path_buf(),
        });
    }
    let session = Session::builder()?.commit_from_file(path)?;
    ModelSchema::from_session(&session)
}

/// Converts an ORT outlet into a stable tensor-only schema.
fn outlet_schema(outlet: &Outlet) -> Result<TensorSchema, LayoutError> {
    let ValueType::Tensor {
        ty,
        shape,
        dimension_symbols,
    } = outlet.dtype()
    else {
        return Err(LayoutError::UnsupportedModelSchema {
            reason: format!("{} is not a tensor", outlet.name()),
        });
    };
    let shape = shape
        .iter()
        .zip(dimension_symbols.iter())
        .map(|(dimension, symbol)| {
            if *dimension >= 0 {
                SchemaDimension::Fixed(*dimension)
            } else if symbol.is_empty() {
                SchemaDimension::Dynamic("?".to_owned())
            } else {
                SchemaDimension::Dynamic(symbol.clone())
            }
        })
        .collect();
    Ok(TensorSchema {
        name: outlet.name().to_owned(),
        dtype: ty.to_string(),
        shape,
    })
}

/// Extracts stable model metadata and sorts custom keys.
fn metadata_schema(
    session: &Session,
) -> Result<ModelMetadataSchema, LayoutError> {
    let metadata = session.metadata()?;
    let mut custom = BTreeMap::new();
    for key in metadata.custom_keys()? {
        if let Some(value) = metadata.custom(&key) {
            custom.insert(key, value);
        }
    }
    Ok(ModelMetadataSchema::builder()
        .name(metadata.name())
        .producer(metadata.producer())
        .domain(metadata.domain())
        .description(metadata.description())
        .graph_description(metadata.graph_description())
        .version(metadata.version())
        .custom(custom)
        .build())
}

#[cfg(test)]
mod tests {
    use crate::LayoutError;

    use super::{
        ModelMetadataSchema, ModelSchema, SchemaDimension, TensorSchema,
    };

    /// Verifies a changed bbox width is rejected as an unsupported schema.
    #[test]
    fn changed_output_dimension_is_rejected() {
        let schema = ModelSchema {
            inputs: vec![
                TensorSchema {
                    name: "im_shape".to_owned(),
                    dtype: "f32".to_owned(),
                    shape: vec![
                        SchemaDimension::Dynamic(
                            "DynamicDimension.0".to_owned(),
                        ),
                        SchemaDimension::Fixed(2),
                    ],
                },
                TensorSchema {
                    name: "image".to_owned(),
                    dtype: "f32".to_owned(),
                    shape: vec![
                        SchemaDimension::Dynamic(
                            "DynamicDimension.1".to_owned(),
                        ),
                        SchemaDimension::Fixed(3),
                        SchemaDimension::Fixed(800),
                        SchemaDimension::Fixed(800),
                    ],
                },
                TensorSchema {
                    name: "scale_factor".to_owned(),
                    dtype: "f32".to_owned(),
                    shape: vec![
                        SchemaDimension::Dynamic(
                            "DynamicDimension.2".to_owned(),
                        ),
                        SchemaDimension::Fixed(2),
                    ],
                },
            ],
            outputs: vec![TensorSchema {
                name: "fetch_name_0".to_owned(),
                dtype: "f32".to_owned(),
                shape: vec![
                    SchemaDimension::Dynamic("DynamicDimension.3".to_owned()),
                    SchemaDimension::Fixed(8),
                ],
            }],
            metadata: ModelMetadataSchema::builder()
                .custom(Default::default())
                .build(),
        };

        assert!(matches!(
            schema.validate_pp_doclayout_v3(),
            Err(LayoutError::UnsupportedModelSchema { .. })
        ));
    }
}
