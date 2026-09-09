use std::collections::BTreeMap;
use std::io::Write;

use docparse_config::OutputConfig;
use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};

use crate::{
    Block, DocumentRelation, DocumentRelations, DocumentResult, Evidence,
    PageResult, RenderError,
};

/// Canonical pretty-JSON renderer with configurable presentation visibility.
pub struct JsonRenderer;

impl JsonRenderer {
    /// Serializes the complete deterministic schema without mutating it.
    pub fn render(document: &DocumentResult) -> Result<String, RenderError> {
        serde_json::to_string_pretty(document).map_err(RenderError::from)
    }

    /// Renders a schema-compatible borrowed visibility view without cloning canonical data.
    pub fn render_with_config(
        document: &DocumentResult,
        config: &OutputConfig,
    ) -> Result<String, RenderError> {
        serde_json::to_string_pretty(&ConfiguredDocument { document, config })
            .map_err(RenderError::from)
    }

    /// Streams a schema-compatible borrowed visibility view to the supplied writer.
    pub fn write_with_config<W: Write>(
        document: &DocumentResult,
        config: &OutputConfig,
        writer: W,
    ) -> Result<(), RenderError> {
        serde_json::to_writer_pretty(
            writer,
            &ConfiguredDocument { document, config },
        )
        .map_err(RenderError::from)
    }
}

/// Borrowed top-level schema view carrying dynamic visibility policy.
struct ConfiguredDocument<'a> {
    document: &'a DocumentResult,
    config: &'a OutputConfig,
}

impl Serialize for ConfiguredDocument<'_> {
    /// Serializes every canonical top-level field through borrowed child views.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("DocumentResult", 5)?;
        state
            .serialize_field("schema_version", &self.document.schema_version)?;
        state.serialize_field("context", &self.document.context)?;
        state.serialize_field(
            "pages",
            &ConfiguredPages {
                pages: &self.document.pages,
                config: self.config,
            },
        )?;
        state.serialize_field(
            "relations",
            &ConfiguredRelations {
                relations: &self.document.relations,
                include_evidence: self.config.include_evidence,
            },
        )?;
        state.serialize_field("errors", &self.document.errors)?;
        state.end()
    }
}

/// Borrowed page sequence that applies one shared output policy.
struct ConfiguredPages<'a> {
    pages: &'a [PageResult],
    config: &'a OutputConfig,
}

impl Serialize for ConfiguredPages<'_> {
    /// Serializes pages incrementally without allocating an intermediate collection.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.pages.len()))?;
        for page in self.pages {
            sequence.serialize_element(&ConfiguredPage {
                page,
                config: self.config,
            })?;
        }
        sequence.end()
    }
}

/// Borrowed page view that can hide diagnostics while preserving schema shape.
struct ConfiguredPage<'a> {
    page: &'a PageResult,
    config: &'a OutputConfig,
}

impl Serialize for ConfiguredPage<'_> {
    /// Serializes one page and delegates evidence filtering to its block sequence.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("PageResult", 7)?;
        state.serialize_field("page_number", &self.page.page_number)?;
        state.serialize_field("width", &self.page.width)?;
        state.serialize_field("height", &self.page.height)?;
        state.serialize_field("rotation", &self.page.rotation)?;
        state.serialize_field(
            "blocks",
            &ConfiguredBlocks {
                blocks: &self.page.blocks,
                include_evidence: self.config.include_evidence,
            },
        )?;
        state.serialize_field("warnings", &self.page.warnings)?;
        if self.config.include_diagnostics {
            state.serialize_field("diagnostics", &self.page.diagnostics)?;
        } else {
            // The field remains present because visibility must not alter the public schema.
            state.serialize_field(
                "diagnostics",
                &BTreeMap::<String, String>::new(),
            )?;
        }
        state.end()
    }
}

/// Borrowed block sequence carrying the evidence visibility flag.
struct ConfiguredBlocks<'a> {
    blocks: &'a [Block],
    include_evidence: bool,
}

impl Serialize for ConfiguredBlocks<'_> {
    /// Serializes blocks incrementally without cloning their nested text facts.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.blocks.len()))?;
        for block in self.blocks {
            sequence.serialize_element(&ConfiguredBlock {
                block,
                include_evidence: self.include_evidence,
            })?;
        }
        sequence.end()
    }
}

/// Borrowed block view that can replace evidence with an empty sequence.
struct ConfiguredBlock<'a> {
    block: &'a Block,
    include_evidence: bool,
}

impl Serialize for ConfiguredBlock<'_> {
    /// Serializes all canonical block fields while applying evidence visibility.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct(
            "Block",
            15 + usize::from(!self.block.source_regions.is_empty()),
        )?;
        state.serialize_field("id", &self.block.id)?;
        state.serialize_field("label", &self.block.label)?;
        state.serialize_field("text", &self.block.text)?;
        state.serialize_field("raw_label", &self.block.raw_label)?;
        state.serialize_field("label_source", &self.block.label_source)?;
        state.serialize_field("confidence", &self.block.confidence)?;
        state.serialize_field("bbox", &self.block.bbox)?;
        state.serialize_field("polygon", &self.block.polygon)?;
        state.serialize_field("source_region", &self.block.source_region)?;
        if !self.block.source_regions.is_empty() {
            state.serialize_field(
                "source_regions",
                &self.block.source_regions,
            )?;
        }
        state
            .serialize_field("model_region_id", &self.block.model_region_id)?;
        state.serialize_field("model_order", &self.block.model_order)?;
        state.serialize_field("final_order", &self.block.final_order)?;
        if self.include_evidence {
            state.serialize_field("evidence", &self.block.evidence)?;
        } else {
            let empty: &[Evidence] = &[];
            state.serialize_field("evidence", &empty)?;
        }
        state.serialize_field("semantic_hints", &self.block.semantic_hints)?;
        state.serialize_field("lines", &self.block.lines)?;
        state.end()
    }
}

/// Borrowed relation collection carrying the evidence visibility flag.
struct ConfiguredRelations<'a> {
    relations: &'a DocumentRelations,
    include_evidence: bool,
}

impl Serialize for ConfiguredRelations<'_> {
    /// Serializes the relation wrapper with a filtered borrowed sequence.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("DocumentRelations", 1)?;
        state.serialize_field(
            "relations",
            &ConfiguredRelationSequence {
                relations: &self.relations.relations,
                include_evidence: self.include_evidence,
            },
        )?;
        state.end()
    }
}

/// Borrowed document-relation sequence carrying the evidence visibility flag.
struct ConfiguredRelationSequence<'a> {
    relations: &'a [DocumentRelation],
    include_evidence: bool,
}

impl Serialize for ConfiguredRelationSequence<'_> {
    /// Serializes relations incrementally without cloning node references.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence =
            serializer.serialize_seq(Some(self.relations.len()))?;
        for relation in self.relations {
            sequence.serialize_element(&ConfiguredRelation {
                relation,
                include_evidence: self.include_evidence,
            })?;
        }
        sequence.end()
    }
}

/// Borrowed relation view that can replace evidence with an empty sequence.
struct ConfiguredRelation<'a> {
    relation: &'a DocumentRelation,
    include_evidence: bool,
}

impl Serialize for ConfiguredRelation<'_> {
    /// Serializes all canonical relation fields while applying evidence visibility.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("DocumentRelation", 5)?;
        state.serialize_field("kind", &self.relation.kind)?;
        state.serialize_field("source", &self.relation.source)?;
        state.serialize_field("target", &self.relation.target)?;
        state.serialize_field("score", &self.relation.score)?;
        if self.include_evidence {
            state.serialize_field("evidence", &self.relation.evidence)?;
        } else {
            let empty: &[Evidence] = &[];
            state.serialize_field("evidence", &empty)?;
        }
        state.end()
    }
}
