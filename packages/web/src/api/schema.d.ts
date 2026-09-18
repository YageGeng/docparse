/** Generated from docparse-server's utoipa OpenAPI document. Do not edit by hand. */
export interface paths {
    "/api/v1/docparse/docs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Browse and try the API using Scalar with the same generated OpenAPI document. */
        get: operations["scalar"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/docparse/health": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Reports process liveness using the same ordinary JSON response shape. */
        get: operations["health"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/docparse/jobs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Streams the sole PDF field to shared storage and acknowledges only a committed, idempotent task.
         * @description Submit exactly one multipart file field containing a PDF. Idempotency-Key is also the job UUID: reuse it after a lost response. Identical content returns the existing visible job; different content or a deleted task returns 4091001. Input is persisted before HTTP 202. Upload size and timeout are deployment settings (defaults: 512 MiB and 300 seconds).
         */
        post: operations["upload"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/docparse/jobs/delete": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Removes a completed task and its result while retaining the shared original PDF and internal cursor anchor.
         * @description Persist deletion of a succeeded or failed task before removing its result JSON. Return 200 after cleanup or 202 while durable cleanup remains pending. Every server role retries cleanup after restart and periodically. Preserve the shared original PDF. Repeated deletion succeeds; queued/running tasks cannot be deleted.
         */
        post: operations["delete"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/docparse/jobs/events": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Replays the latest durable snapshot on every connection and polls revisions without owning the parse task.
         * @description SSE event `job` contains ApiResponse<JobSnapshot> JSON; its id is the persisted version. Every connection replays the latest snapshot, even with Last-Event-ID. Intermediate progress is coalesced rather than retained as event history. Terminal snapshots close the stream: clients should close EventSource on succeeded/failed. Disconnects do not cancel parsing. Heartbeats are sent every 15 seconds; database failure after HTTP 200 is sent as an `error` event containing ApiErrorResponse, then the stream closes.
         */
        get: operations["events"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/docparse/jobs/list": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Lists durable tasks by descending creation time with bounded cursor pagination and optional filters.
         * @description List persisted tasks newest first. Limit defaults to 20 and must be 1–100. Pass next_cursor with the same filters to continue. Search matches literal filename text case-insensitively. Old jobs can have null filename and size_bytes.
         */
        get: operations["list"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/docparse/jobs/result": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Streams the immutable successful JSON envelope from shared storage rather than loading it into API memory.
         * @description Read the completed document as JSON (default), one JSON page with page=N, or cached Markdown. Responses support gzip and If-None-Match revalidation. JSON includes both LaTeX and Markdown for every recognized formula. Markdown is a presentation of the stored result; no inference is repeated.
         */
        get: operations["result"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/docparse/jobs/source": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Streams the immutable uploaded PDF with standard HEAD and byte-range support for lazy browser previews.
         * @description Read the original uploaded PDF independently of parse status. Supports HEAD, single byte ranges and conditional requests through the file service. Storage paths never derive from the uploaded filename.
         */
        get: operations["source"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/docparse/jobs/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Retrieves current durable progress without requiring affinity to the accepting API instance. */
        get: operations["status"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/docparse/openapi.json": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Download the OpenAPI document generated from the registered handlers and response types. */
        get: operations["openapi"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/docparse/ready": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Allows a load balancer to stop routing to draining instances or unavailable shared dependencies.
         * @description Check task schema availability, writable shared storage, and whether this instance is draining. API-only instances do not load models or verify that a remote worker is available.
         */
        get: operations["ready"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        /** @description Stable failure shape shared with WisLand's clients. */
        ApiErrorResponse: {
            error: components["schemas"]["ErrorDetail"];
            success: boolean;
        };
        /** @description WisLand-compatible successful JSON envelope, also used inside SSE data messages. */
        ApiResponse_JobJsonResult: {
            /** @description JSON result shape depends on whether the page query parameter is present. */
            data: components["schemas"]["DocumentResult"] | components["schemas"]["Pagenation_PageResult"];
            message: string;
            success: boolean;
        };
        /** @description WisLand-compatible successful JSON envelope, also used inside SSE data messages. */
        ApiResponse_JobList: {
            /** @description A history page includes a cursor only when another matching row exists. */
            data: {
                items: components["schemas"]["JobSnapshot"][];
                /** Format: uuid */
                next_cursor?: string | null;
            };
            message: string;
            success: boolean;
        };
        /** @description WisLand-compatible successful JSON envelope, also used inside SSE data messages. */
        ApiResponse_JobSnapshot: {
            /** @description Public progress snapshots omit storage names, lease tokens, and other worker-only fields. */
            data: {
                /** Format: int32 */
                attempts: number;
                /** Format: date-time */
                created_at: string;
                /**
                 * Format: int64
                 * @description Latest completed attempt's elapsed milliseconds, including parsing and result publication; excludes upload, queueing, and the final database update.
                 */
                duration_ms?: number | null;
                error?: string | null;
                filename?: string | null;
                /** Format: uuid */
                id: string;
                progress?: null | components["schemas"]["ParseProgress"];
                /** Format: int64 */
                size_bytes?: number | null;
                /** @description The shared task enum retains the existing lowercase wire values. */
                status: components["schemas"]["JobStatus"];
                /** Format: date-time */
                updated_at: string;
                /** Format: int64 */
                version: number;
            };
            message: string;
            success: boolean;
        };
        /** @description WisLand-compatible successful JSON envelope, also used inside SSE data messages. */
        ApiResponse_String: {
            data: string;
            message: string;
            success: boolean;
        };
        /** @description A baseline segment in canonical viewport coordinates. */
        Baseline: {
            end: components["schemas"]["Point"];
            start: components["schemas"]["Point"];
        };
        /** @description A finite axis-aligned box using left, top, right, and bottom coordinates. */
        Bbox: {
            /** Format: double */
            bottom: number;
            /** Format: double */
            left: number;
            /** Format: double */
            right: number;
            /** Format: double */
            top: number;
        };
        /** @description One final semantic block that exclusively owns its lines. */
        Block: {
            bbox: components["schemas"]["Bbox"];
            /** Format: double */
            confidence?: number | null;
            evidence: components["schemas"]["Evidence"][];
            /** Format: int32 */
            final_order: number;
            id: components["schemas"]["BlockId"];
            label: components["schemas"]["LayoutLabel"];
            label_source: components["schemas"]["LabelSource"];
            lines: components["schemas"]["Line"][];
            /** @description Paragraph presentation with recognized inline formulas; original text and source items remain unchanged. */
            markdown?: string | null;
            /** Format: int64 */
            model_order?: number | null;
            model_region_id?: null | components["schemas"]["ModelRegionId"];
            polygon?: null | components["schemas"]["Polygon"];
            raw_label?: string | null;
            semantic_hints: {
                [key: string]: string;
            };
            source_region?: null | components["schemas"]["SourceRegionEvidence"];
            /** @description All contributing regions for a merged layout; empty for legacy or unmerged blocks. */
            source_regions?: components["schemas"]["SourceRegionEvidence"][];
            table?: null | components["schemas"]["Table"];
            text: string;
        };
        /** @description Stable identity for one final block. */
        BlockId: string;
        /** @description Immutable document-level facts shared by page analysis. */
        DocumentContext: {
            /** Format: double */
            body_font_size?: number | null;
            heading_font_sizes: number[];
            metadata: {
                [key: string]: string;
            };
            model_revision?: string | null;
            /** Format: int32 */
            page_count: number;
            page_number_pattern?: string | null;
            repeated_footer_fingerprints: string[];
            repeated_header_fingerprints: string[];
        };
        /** @description One deterministic non-owning document relation. */
        DocumentRelation: {
            evidence: components["schemas"]["Evidence"][];
            kind: components["schemas"]["RelationKind"];
            /** Format: double */
            score?: number | null;
            source: components["schemas"]["NodeRef"];
            target: components["schemas"]["NodeRef"];
        };
        /** @description Sidecar relations that never modify page ownership or reading order. */
        DocumentRelations: {
            relations: components["schemas"]["DocumentRelation"][];
        };
        /** @description Canonical complete document aggregate. */
        DocumentResult: {
            context: components["schemas"]["DocumentContext"];
            errors: components["schemas"]["PageError"][];
            pages: components["schemas"]["PageResult"][];
            relations: components["schemas"]["DocumentRelations"];
            /** @example 2.0 */
            schema_version: string;
        };
        /** @description Response details carry the numeric code and the error's own formatted explanation. */
        ErrorDetail: {
            /** Format: int64 */
            code: number;
            message: string;
        };
        /** @description Generic deterministic evidence attached to a result decision. */
        Evidence: {
            details: {
                [key: string]: string;
            };
            kind: string;
            /** Format: double */
            score?: number | null;
        };
        /** @description Stable identity for one residual XY-cut region. */
        FallbackRegionId: string;
        /** @description Recognized mathematics with non-owning references to its original layout and text. */
        FormulaResult: {
            /** @description Original formula extent in viewport points. */
            bbox: components["schemas"]["Bbox"];
            block_id?: null | components["schemas"]["BlockId"];
            crop_bbox?: null | components["schemas"]["Bbox"];
            /** @description Actual model/backend identity, including any documented compatibility executor. */
            engine?: string;
            /** @description Explicit recognition failure; original text remains available independently. */
            error?: string | null;
            /** @description Stable identity derived from the original layout detection row. */
            id: components["schemas"]["ModelRegionId"];
            /** @description Original inline_formula or display_formula classification. */
            label: components["schemas"]["LayoutLabel"];
            /** @description Decoded LaTeX without outer Markdown math delimiters; null on failure. */
            latex?: string | null;
            line_id?: null | components["schemas"]["LineId"];
            /** @description LaTeX wrapped with the original inline or display math delimiters. */
            markdown?: string | null;
            /** @description Zero-based row and column for formulas inside a recovered table. */
            table_cell?: [
                number,
                number
            ] | null;
            text_item_range?: null | components["schemas"]["TextItemRange"];
            /** @description Exact UTF-8 source slices; boundary prose in the same TextItem remains outside the replacement. */
            text_spans?: components["schemas"]["TableTextSpan"][];
        };
        /**
         * @description Records whether polygon geometry is factual or derived from a bounding box.
         * @enum {string}
         */
        GeometrySource: "ModelPolygon" | "DerivedFromBbox";
        /**
         * @description Completeness of text located beneath one inline formula region.
         * @enum {string}
         */
        InlineContentStatus: "Complete" | "Partial" | "Missing";
        /** @description A non-owning inline formula annotation attached to one line. */
        InlineSpan: {
            bbox: components["schemas"]["Bbox"];
            /** Format: double */
            confidence?: number | null;
            content_status: components["schemas"]["InlineContentStatus"];
            extracted_text?: string | null;
            label: components["schemas"]["LayoutLabel"];
            polygon?: null | components["schemas"]["Polygon"];
            text_item_range: components["schemas"]["TextItemRange"];
        };
        /** @description JSON result shape depends on whether the page query parameter is present. */
        JobJsonResult: components["schemas"]["DocumentResult"] | components["schemas"]["Pagenation_PageResult"];
        /** @description A history page includes a cursor only when another matching row exists. */
        JobList: {
            items: components["schemas"]["JobSnapshot"][];
            /** Format: uuid */
            next_cursor?: string | null;
        };
        /** @description Public progress snapshots omit storage names, lease tokens, and other worker-only fields. */
        JobSnapshot: {
            /** Format: int32 */
            attempts: number;
            /** Format: date-time */
            created_at: string;
            /**
             * Format: int64
             * @description Latest completed attempt's elapsed milliseconds, including parsing and result publication; excludes upload, queueing, and the final database update.
             */
            duration_ms?: number | null;
            error?: string | null;
            filename?: string | null;
            /** Format: uuid */
            id: string;
            progress?: null | components["schemas"]["ParseProgress"];
            /** Format: int64 */
            size_bytes?: number | null;
            /** @description The shared task enum retains the existing lowercase wire values. */
            status: components["schemas"]["JobStatus"];
            /** Format: date-time */
            updated_at: string;
            /** Format: int64 */
            version: number;
        };
        /**
         * @description Shares one task-state vocabulary across queries and API snapshots while retaining the deployed varchar values.
         * @enum {string}
         */
        JobStatus: "queued" | "running" | "succeeded" | "failed";
        /**
         * @description Origin of one final block's primary semantic label.
         * @enum {string}
         */
        LabelSource: "Model" | "Pdf" | "Heuristic" | "Fallback";
        /** @description Fixed model labels, parser-derived semantics, and forward-compatible unknown labels. */
        LayoutLabel: "abstract" | "algorithm" | "aside_text" | "chart" | "content" | "display_formula" | "doc_title" | "figure_title" | "footer" | "footer_image" | "footnote" | "formula_number" | "header" | "header_image" | "image" | "inline_formula" | "number" | "paragraph_title" | "reference" | "reference_content" | "seal" | "table" | "text" | "vertical_text" | "vision_footnote" | "watermark" | {
            unknown: string;
        };
        /** @description One final line that exclusively owns its text items. */
        Line: {
            baseline?: null | components["schemas"]["Baseline"];
            bbox: components["schemas"]["Bbox"];
            direction: components["schemas"]["WritingDirection"];
            id: components["schemas"]["LineId"];
            inline_spans: components["schemas"]["InlineSpan"][];
            /** Format: double */
            model_region_coverage?: number | null;
            /** Format: double */
            rotation: number;
            text: string;
            text_items: components["schemas"]["TextItem"][];
        };
        /** @description Stable identity for one final line. */
        LineId: string;
        /** @description Stable identity for one model detection row. */
        ModelRegionId: string;
        /** @description Non-owning reference used by document-level relations. */
        NodeRef: {
            block_id: components["schemas"]["BlockId"];
            line_id?: null | components["schemas"]["LineId"];
            /** Format: int32 */
            page_number: number;
        };
        /** @description Stable page failure that never embeds local paths or source bytes. */
        PageError: {
            code: string;
            message: string;
            /** Format: int32 */
            page_number: number;
            stage: string;
        };
        /** @description One page's canonical nested result. */
        PageResult: {
            blocks: components["schemas"]["Block"][];
            diagnostics: {
                [key: string]: string;
            };
            /** @description Formula recognition outputs retain both LaTeX and Markdown under every JSON visibility policy. */
            formulas?: components["schemas"]["FormulaResult"][];
            /** Format: double */
            height: number;
            /** Format: int32 */
            page_number: number;
            /** @description Original unusable PDF facts replaced by confident OCR; excluded from reading order, retained for audit. */
            replaced_native_text?: components["schemas"]["TextItem"][];
            /** Format: int32 */
            rotation: number;
            warnings: components["schemas"]["PageWarning"][];
            /** Format: double */
            width: number;
        };
        /** @description Stable warning emitted for one page without discarding available content. */
        PageWarning: {
            code: string;
            message: string;
            stage: string;
        };
        /** @description A selected document page with total page count and parsing errors. */
        Pagenation_PageResult: {
            /** @description Document parsing errors remain visible when a requested page is missing. */
            errors: components["schemas"]["PageError"][];
            /** @description One page's canonical nested result. */
            page?: {
                blocks: components["schemas"]["Block"][];
                diagnostics: {
                    [key: string]: string;
                };
                /** @description Formula recognition outputs retain both LaTeX and Markdown under every JSON visibility policy. */
                formulas?: components["schemas"]["FormulaResult"][];
                /** Format: double */
                height: number;
                /** Format: int32 */
                page_number: number;
                /** @description Original unusable PDF facts replaced by confident OCR; excluded from reading order, retained for audit. */
                replaced_native_text?: components["schemas"]["TextItem"][];
                /** Format: int32 */
                rotation: number;
                warnings: components["schemas"]["PageWarning"][];
                /** Format: double */
                width: number;
            };
            /**
             * Format: int32
             * @description Total source PDF pages, including pages that failed parsing.
             */
            page_count: number;
        };
        /** @description Actual document pipeline boundaries, independent of execution speed or platform. */
        ParseProgress: {
            /** @enum {string} */
            stage: "opening";
        } | {
            /** Format: int32 */
            completed: number;
            /** @enum {string} */
            stage: "scanning";
            /** Format: int32 */
            total: number;
        } | {
            /** Format: int32 */
            completed: number;
            /** @enum {string} */
            stage: "analyzing";
            /** Format: int32 */
            total: number;
        } | {
            /** @enum {string} */
            stage: "linking";
            /** Format: int32 */
            total: number;
        } | {
            /** @enum {string} */
            stage: "complete";
            /** Format: int32 */
            total: number;
        };
        /** @description Stable PDF provenance that excludes temporary handles and pointer values. */
        PdfProvenance: {
            char_codes: number[];
            generated_space: boolean;
            link?: string | null;
            /** Format: int32 */
            mcid?: number | null;
            strike: boolean;
            /** Format: int32 */
            text_object_index?: number | null;
            unicode_mapping: components["schemas"]["UnicodeMappingStatus"];
        };
        /** @description Multipart contract for a streamed upload; the handler continues to process chunks instead of buffering this DTO. */
        PdfUpload: {
            /**
             * Format: binary
             * @description The sole PDF file, beginning with the %PDF- signature.
             */
            file: string;
        };
        /** @description A point in one explicitly documented two-dimensional coordinate space. */
        Point: {
            /** Format: double */
            x: number;
            /** Format: double */
            y: number;
        };
        Polygon: {
            /** Format: double */
            x: number;
            /** Format: double */
            y: number;
        }[];
        /**
         * @description Supported document-level relation categories.
         * @enum {string}
         */
        RelationKind: "RepeatedChrome" | "ParagraphContinuation" | "HeadingHierarchy" | "TableContinuationCandidate";
        /**
         * @description Explicit text repair evidence retained beside the original text fact.
         * @enum {string}
         */
        RepairAction: "RemovedControl" | "EncodedHyphen" | "MergedFragment" | "GlyphNameRecovery" | "FontCmapRecovery" | "GlyphOutlineRecovery" | "GlyphComposition" | "LigatureExpansion" | "PunctuationNormalization" | "OcrSpacing" | "OcrNativeOverlap";
        /** @description Original model or fallback region geometry retained beside final content bounds. */
        SourceRegionEvidence: {
            bbox: components["schemas"]["Bbox"];
            /** Format: double */
            confidence?: number | null;
            fallback_region_id?: null | components["schemas"]["FallbackRegionId"];
            geometry_source: components["schemas"]["GeometrySource"];
            label?: null | components["schemas"]["LayoutLabel"];
            /** Format: int64 */
            model_order?: number | null;
            model_region_id?: null | components["schemas"]["ModelRegionId"];
            polygon?: null | components["schemas"]["Polygon"];
        };
        /** @description Structured view of one table block; canonical source text stays in Block.lines. */
        Table: {
            cells: components["schemas"]["TableCell"][];
            column_count: number;
            row_count: number;
            /** @description Records the accepted reconstruction path, independently of layout detection provenance. */
            source: components["schemas"]["TableStructureSource"];
        };
        /** @description One logical cell; covered rowspan/colspan positions never become duplicate cells. */
        TableCell: {
            bbox?: null | components["schemas"]["Bbox"];
            column: number;
            column_span: number;
            is_header: boolean;
            lines: components["schemas"]["TableCellLine"][];
            /** @description Formula-enriched presentation; canonical text and source spans remain unchanged. */
            markdown?: string | null;
            row: number;
            row_span: number;
            text: string;
        };
        /** @description One physical line inside a cell, preserving its source references in reading order. */
        TableCellLine: {
            bbox: components["schemas"]["Bbox"];
            spans: components["schemas"]["TableTextSpan"][];
            text: string;
        };
        /**
         * @description Evidence used to recover a table's row/column topology.
         * @enum {string}
         */
        TableStructureSource: "tagged_pdf" | "ruled" | "text_alignment" | "external_tsr";
        /** @description A non-owning slice of one source item; offsets are UTF-8 bytes, not character ordinals. */
        TableTextSpan: {
            bbox: components["schemas"]["Bbox"];
            byte_range: {
                end: number;
                start: number;
            };
            text_item_id: components["schemas"]["TextItemId"];
        };
        /** @description One continuous native or OCR text fact. */
        TextItem: {
            baseline?: null | components["schemas"]["Baseline"];
            bbox: components["schemas"]["Bbox"];
            /** Format: double */
            confidence?: number | null;
            /** Format: int32 */
            extraction_order: number;
            /** Format: int32 */
            final_order: number;
            id: components["schemas"]["TextItemId"];
            polygon?: null | components["schemas"]["Polygon"];
            provenance?: null | components["schemas"]["PdfProvenance"];
            raw_bbox?: null | components["schemas"]["Bbox"];
            raw_text: string;
            repair_actions: components["schemas"]["RepairAction"][];
            /** Format: double */
            rotation: number;
            source: components["schemas"]["TextSource"];
            style?: null | components["schemas"]["TextStyle"];
            watermark?: null | components["schemas"]["WatermarkSource"];
        };
        /** @description Stable identity for one native or OCR text fact. */
        TextItemId: string;
        /** @description Half-open text-item ordinal range inside one final line. */
        TextItemRange: {
            end: number;
            start: number;
        };
        /**
         * @description Origin of one text fact.
         * @enum {string}
         */
        TextSource: "Native" | "Ocr";
        /** @description Rich font and paint facts aggregated over one text item. */
        TextStyle: {
            bold: boolean;
            fill_color?: number[] | null;
            /** Format: int32 */
            flags?: number | null;
            /** Format: double */
            font_ascent?: number | null;
            /** Format: double */
            font_descent?: number | null;
            /** Format: double */
            font_height?: number | null;
            font_name?: string | null;
            /** Format: double */
            font_size?: number | null;
            font_size_estimated: boolean;
            italic: boolean;
            monospace: boolean;
            stroke_color?: number[] | null;
            text_matrix?: number[] | null;
            /** Format: int32 */
            weight?: number | null;
        };
        /**
         * @description Whether PDFium could map source character codes to Unicode.
         * @enum {string}
         */
        UnicodeMappingStatus: "Complete" | "Partial" | "Missing";
        /**
         * @description Positive watermark evidence; absence leaves an ordinary text fact eligible for fusion.
         * @enum {string}
         */
        WatermarkSource: "pdf_marked_content" | "text_pattern";
        /**
         * @description Final inline ordering direction for a line.
         * @enum {string}
         */
        WritingDirection: "LeftToRight" | "RightToLeft" | "Vertical";
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
    scalar: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Scalar API reference */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "text/html": string;
                };
            };
        };
    };
    health: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Process is alive */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    /**
                     * @example {
                     *       "data": "ok",
                     *       "message": "Success",
                     *       "success": true
                     *     }
                     */
                    "application/json": components["schemas"]["ApiResponse_String"];
                };
            };
        };
    };
    upload: {
        parameters: {
            query?: never;
            header: {
                /** @description Required UUID identifying this submission and all safe retries */
                "Idempotency-Key": string;
            };
            path?: never;
            cookie?: never;
        };
        /** @description Exactly one binary file field */
        requestBody: {
            content: {
                "multipart/form-data": components["schemas"]["PdfUpload"];
            };
        };
        responses: {
            /** @description Durable task accepted, or matching task already exists */
            202: {
                headers: {
                    /** @description Task status URL */
                    Location?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiResponse_JobSnapshot"];
                };
            };
            /** @description Malformed multipart headers (400000), invalid upload (4001001), or invalid idempotency key (4001003) */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Upload timeout (4081001) */
            408: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Idempotency key refers to different PDF content or a deleted task (4091001) */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description PDF or multipart request exceeds its configured limit (4131001) */
            413: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Upload capacity is full (4291001) */
            429: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Internal error (500000) */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Instance draining, database unavailable, or shared storage unavailable (5031001-5031003) */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    delete: {
        parameters: {
            query: {
                /** @description UUID returned on submission, equal to the upload's Idempotency-Key. */
                id: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted task UUID */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiResponse_String"];
                };
            };
            /** @description Task deleted; result cleanup will be retried automatically */
            202: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiResponse_String"];
                };
            };
            /** @description Invalid task UUID (4001002) */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Task not found (4041001) */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Task is still queued or running (4091004) */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Deletion intent could not be confirmed in the database (5031002) */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    events: {
        parameters: {
            query: {
                /** @description UUID returned on submission, equal to the upload's Idempotency-Key. */
                id: string;
            };
            header?: {
                /** @description May be supplied by EventSource; the latest snapshot is always replayed */
                "Last-Event-ID"?: string | null;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description SSE frames carrying task snapshots or errors */
            200: {
                headers: {
                    /** @description no-cache, no-transform */
                    "Cache-Control"?: string;
                    /** @description no */
                    "X-Accel-Buffering"?: string;
                    [name: string]: unknown;
                };
                content: {
                    /**
                     * @example event: job
                     *     id: 4
                     *     data: {"data":{"id":"07bd9078-a15f-4b41-bbca-e341047db61e","status":"succeeded","version":4,"attempts":1,"progress":{"stage":"complete","total":3},"error":null},"success":true,"message":"Success"}
                     */
                    "text/event-stream": string;
                };
            };
            /** @description Invalid task UUID (4001002) */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Task not found (4041001) */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Internal error before streaming starts (500000) */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Task database unavailable before streaming starts (5031002) */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    list: {
        parameters: {
            query?: {
                cursor?: string;
                limit?: number;
                status?: components["schemas"]["JobStatus"];
                search?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Durable task history */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiResponse_JobList"];
                };
            };
            /** @description Invalid query or cursor (4001002) */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Task database unavailable (5031002) */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    result: {
        parameters: {
            query: {
                /** @description UUID returned by the upload endpoint. */
                id: string;
                /** @description Defaults to json; markdown returns text/markdown rather than a JSON envelope. */
                format?: "json" | "markdown";
                /** @description Optional one-based page number; available only for JSON and preserves full-document downloads when omitted. */
                page?: number;
            };
            header?: {
                /** @description Revalidate an immutable result representation */
                "If-None-Match"?: string | null;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Canonical JSON envelope or complete document Markdown */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiResponse_JobJsonResult"];
                    "text/markdown": string;
                };
            };
            /** @description Result representation is unchanged */
            304: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Invalid task UUID (4001002) */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Task not found (4041001) */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Result not ready (4091002) or task failed (4091003) */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Internal result state error (500000) */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Database or shared storage unavailable (5031002-5031003) */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    source: {
        parameters: {
            query: {
                /** @description UUID returned on submission, equal to the upload's Idempotency-Key. */
                id: string;
            };
            header?: {
                /** @description Optional byte range, for example bytes=0-65535 */
                Range?: string | null;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Original PDF */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/pdf": string;
                };
            };
            /** @description Requested PDF byte range */
            206: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/pdf": string;
                };
            };
            /** @description Conditional request is not modified */
            304: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Invalid task UUID (4001002) */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Task not found (4041001) */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Byte range not satisfiable; Content-Range reports the PDF size */
            416: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Database or source file unavailable (5031002-5031003) */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    status: {
        parameters: {
            query: {
                /** @description UUID returned on submission, equal to the upload's Idempotency-Key. */
                id: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Latest persisted task snapshot */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiResponse_JobSnapshot"];
                };
            };
            /** @description Invalid task UUID (4001002) */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Task not found (4041001) */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Internal error (500000) */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Task database unavailable (5031002) */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    openapi: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description OpenAPI 3.1 document */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": Record<string, never>;
                };
            };
        };
    };
    ready: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Shared dependencies ready */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    /**
                     * @example {
                     *       "data": "ready",
                     *       "message": "Success",
                     *       "success": true
                     *     }
                     */
                    "application/json": components["schemas"]["ApiResponse_String"];
                };
            };
            /** @description Internal readiness error (500000) */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Instance draining, database/schema unavailable, or shared storage unavailable (5031001-5031003) */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
}
