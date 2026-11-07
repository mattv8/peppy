//! Deterministic client-side planning for gateways that can resize MMS images.

pub const MMS_OVERHEAD_BYTES: u64 = 16 * 1024;
pub const MIN_IMAGE_BUDGET_BYTES: u64 = 90 * 1024;
pub const MAX_MMS_ATTACHMENTS: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MmsImage {
    pub id: String,
    pub bytes: u64,
    pub media_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MmsPlan {
    Single,
    Split {
        caption: bool,
        image_groups: Vec<Vec<String>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MmsPlanError {
    TooLarge,
    NonResizableImageTooLarge { image_id: String },
}

pub fn plan_mms(
    limit_bytes: u64,
    gateway_mms_content_version: u32,
    text: &str,
    images: &[MmsImage],
) -> Result<MmsPlan, MmsPlanError> {
    let fits_as_is = images
        .iter()
        .try_fold(text.len() as u64, |total, image| {
            total.checked_add(image.bytes)
        })
        .is_some_and(|total| total <= limit_bytes);
    if fits_as_is {
        return Ok(MmsPlan::Single);
    }
    if gateway_mms_content_version < 4 {
        return Err(MmsPlanError::TooLarge);
    }
    if let Some(image) = images
        .iter()
        .find(|image| !is_resizable(&image.media_type) && image.bytes > MIN_IMAGE_BUDGET_BYTES)
    {
        return Err(MmsPlanError::NonResizableImageTooLarge {
            image_id: image.id.clone(),
        });
    }

    let per_message =
        usize::try_from(limit_bytes.saturating_sub(MMS_OVERHEAD_BYTES) / MIN_IMAGE_BUDGET_BYTES)
            .unwrap_or(usize::MAX)
            .clamp(1, MAX_MMS_ATTACHMENTS);
    if images.len() <= per_message {
        return Ok(MmsPlan::Single);
    }

    let group_count = images.len().div_ceil(per_message);
    let group_size = images.len() / group_count;
    let larger_group_count = images.len() % group_count;
    let mut offset = 0;
    let mut image_groups = Vec::with_capacity(group_count);
    for index in 0..group_count {
        let size = group_size + usize::from(index < larger_group_count);
        let group = images[offset..offset + size]
            .iter()
            .map(|image| image.id.clone())
            .collect();
        image_groups.push(group);
        offset += size;
    }
    Ok(MmsPlan::Split {
        caption: !text.trim().is_empty(),
        image_groups,
    })
}

fn is_resizable(media_type: &str) -> bool {
    [
        "image/jpeg",
        "image/png",
        "image/webp",
        "image/heic",
        "image/heif",
    ]
    .iter()
    .any(|resizable| media_type.eq_ignore_ascii_case(resizable))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct VectorFile {
        vectors: Vec<Vector>,
    }

    #[derive(Deserialize)]
    struct Vector {
        name: String,
        limit: u64,
        version: u32,
        text: String,
        images: Vec<VectorImage>,
        expect: Expectation,
    }

    #[derive(Deserialize)]
    struct VectorImage {
        bytes: u64,
        #[serde(rename = "type")]
        media_type: String,
    }

    #[derive(Deserialize)]
    struct Expectation {
        kind: String,
        caption: Option<bool>,
        groups: Option<Vec<usize>>,
    }

    #[test]
    fn matches_shared_vectors() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mms_plan_vectors.json");
        let vectors: VectorFile =
            serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
        for vector in vectors.vectors {
            let images = vector
                .images
                .into_iter()
                .enumerate()
                .map(|(index, image)| MmsImage {
                    id: index.to_string(),
                    bytes: image.bytes,
                    media_type: image.media_type,
                })
                .collect::<Vec<_>>();
            let result = plan_mms(vector.limit, vector.version, &vector.text, &images);
            match vector.expect.kind.as_str() {
                "error" => assert!(result.is_err(), "{}", vector.name),
                "single" => assert_eq!(result.unwrap(), MmsPlan::Single, "{}", vector.name),
                "split" => match result.unwrap() {
                    MmsPlan::Split {
                        caption,
                        image_groups,
                    } => {
                        assert_eq!(Some(caption), vector.expect.caption, "{}", vector.name);
                        assert_eq!(
                            image_groups.iter().map(Vec::len).collect::<Vec<_>>(),
                            vector.expect.groups.unwrap(),
                            "{}",
                            vector.name
                        );
                    }
                    MmsPlan::Single => panic!("{}", vector.name),
                },
                kind => panic!("unknown expectation {kind}"),
            }
        }
    }

    #[test]
    fn checked_size_overflow_plans_resizable_images() {
        let images = [
            MmsImage {
                id: "first".into(),
                bytes: u64::MAX,
                media_type: "image/jpeg".into(),
            },
            MmsImage {
                id: "second".into(),
                bytes: 1,
                media_type: "image/png".into(),
            },
        ];

        assert_eq!(plan_mms(300 * 1024, 4, "", &images), Ok(MmsPlan::Single));
    }

    #[test]
    fn tiny_limits_and_budget_boundary_are_safe() {
        let image = MmsImage {
            id: "gif".into(),
            bytes: MIN_IMAGE_BUDGET_BYTES,
            media_type: "image/gif".into(),
        };
        assert_eq!(plan_mms(0, 4, "x", &[image]), Ok(MmsPlan::Single));
    }

    #[test]
    fn split_groups_preserve_order_with_larger_earlier_groups() {
        let images = (0..7)
            .map(|index| MmsImage {
                id: index.to_string(),
                bytes: 1_000_000,
                media_type: "image/jpeg".into(),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            plan_mms(300 * 1024, 4, "caption", &images),
            Ok(MmsPlan::Split {
                caption: true,
                image_groups: vec![
                    vec!["0".into(), "1".into(), "2".into()],
                    vec!["3".into(), "4".into()],
                    vec!["5".into(), "6".into()],
                ],
            })
        );
    }
}
