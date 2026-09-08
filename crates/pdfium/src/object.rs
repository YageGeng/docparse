use crate::{Page, PointF, TextObjectIdentity, ffi};

/// Borrowed text-object identity and copied PDFium facts in raw page coordinates.
#[derive(Debug, Clone)]
pub struct TextObjectFacts<'page> {
    pub identity: TextObjectIdentity<'page>,
    pub quad: Option<[PointF; 4]>,
    pub watermark: bool,
}

impl Page<'_, '_> {
    /// Reads explicit content marks and tight bounds, including inherited form transforms.
    pub fn text_object_facts(&self) -> Vec<TextObjectFacts<'_>> {
        let mut facts = Vec::new();
        // SAFETY: this page owns every object visited while these facts are borrowed.
        let count = unsafe { ffi!(FPDFPage_CountObjects(self.handle)) };
        for index in 0..count {
            // SAFETY: the index is bounded by this live page's object count.
            let object =
                unsafe { ffi!(FPDFPage_GetObject(self.handle, index)) };
            collect(
                object,
                [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                false,
                0,
                &mut facts,
            );
        }
        facts
    }
}

/// Reads only positive watermark declarations, never treating an ordinary Artifact as one.
fn has_watermark_mark(object: pdfium_sys::FPDF_PAGEOBJECT) -> bool {
    // SAFETY: callers only pass objects borrowed from the live page traversal.
    let count = unsafe { ffi!(FPDFPageObj_CountMarks(object)) };
    for index in 0..count {
        // SAFETY: the index is within the object's mark count; marks share its lifetime.
        let mark = unsafe { ffi!(FPDFPageObj_GetMark(object, index as _)) };
        if mark.is_null() {
            continue;
        }
        let mut name = [0_u16; 64];
        let mut length = 0;
        // SAFETY: the writable buffer size is expressed in bytes as required by PDFium.
        let read = unsafe {
            ffi!(FPDFPageObjMark_GetName(
                mark,
                name.as_mut_ptr(),
                128,
                &mut length
            ))
        };
        if read == 0 || length > 128 || length % 2 != 0 {
            continue;
        }
        let name = String::from_utf16_lossy(&name[..length as usize / 2]);
        if name.trim_end_matches('\0') == "Watermark" {
            return true;
        }
        if name.trim_end_matches('\0') != "Artifact" {
            continue;
        }
        let mut subtype = [0_u16; 64];
        // PDFium exposes string-valued mark parameters, but not PDF Name values.
        // An unreadable /Subtype must stay unknown and may be assessed by text rules.
        // SAFETY: mark is live; the key is NUL-terminated and the output buffer is sized in bytes.
        let read = unsafe {
            ffi!(FPDFPageObjMark_GetParamStringValue(
                mark,
                c"Subtype".as_ptr(),
                subtype.as_mut_ptr(),
                128,
                &mut length
            ))
        };
        if read != 0
            && length <= 128
            && length % 2 == 0
            && String::from_utf16_lossy(&subtype[..length as usize / 2])
                .trim_end_matches('\0')
                == "Watermark"
        {
            return true;
        }
    }
    false
}

/// Walks nested form objects with a depth bound; no handle escapes the enclosing page lifetime.
fn collect<'page>(
    object: pdfium_sys::FPDF_PAGEOBJECT,
    outer: [f64; 6],
    inherited: bool,
    depth: usize,
    facts: &mut Vec<TextObjectFacts<'page>>,
) {
    if object.is_null() || depth > 32 {
        return;
    }
    let watermark = inherited || has_watermark_mark(object);
    // SAFETY: the object is non-null and was borrowed from a live page or form.
    let kind = unsafe { ffi!(FPDFPageObj_GetType(object)) };
    let [a, b, c, d, e, f] = outer;
    if kind == pdfium_sys::FPDF_PAGEOBJ_TEXT as i32 {
        let Some(identity) = TextObjectIdentity::from_handle(object) else {
            return;
        };
        let mut raw = pdfium_sys::FS_QUADPOINTSF::default();
        // SAFETY: PDFium writes the stack-owned output and retains no pointer to it.
        let found =
            unsafe { ffi!(FPDFPageObj_GetRotatedBounds(object, &mut raw)) };
        let quad = (found != 0).then(|| {
            [
                (raw.x1, raw.y1),
                (raw.x2, raw.y2),
                (raw.x3, raw.y3),
                (raw.x4, raw.y4),
            ]
            .map(|(x, y)| {
                let (x, y) = (f64::from(x), f64::from(y));
                PointF {
                    x: a * x + c * y + e,
                    y: b * x + d * y + f,
                }
            })
        });
        facts.push(TextObjectFacts {
            identity,
            quad,
            watermark,
        });
    } else if kind == pdfium_sys::FPDF_PAGEOBJ_FORM as i32 {
        let mut matrix = pdfium_sys::FS_MATRIX::default();
        // SAFETY: the live form owns its matrix and every child returned below.
        if unsafe { ffi!(FPDFPageObj_GetMatrix(object, &mut matrix)) } == 0 {
            return;
        }
        let [u, v, w, z, x, y] =
            [matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f]
                .map(f64::from);
        let combined = [
            a * u + c * v,
            b * u + d * v,
            a * w + c * z,
            b * w + d * z,
            a * x + c * y + e,
            b * x + d * y + f,
        ];
        // SAFETY: object was identified as a form, and child indices stay within its count.
        let count = unsafe { ffi!(FPDFFormObj_CountObjects(object)) };
        for index in 0..count {
            // SAFETY: this index belongs to the live form's child object array.
            let child =
                unsafe { ffi!(FPDFFormObj_GetObject(object, index as _)) };
            collect(child, combined, watermark, depth + 1, facts);
        }
    }
}
