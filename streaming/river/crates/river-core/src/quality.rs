use crate::{PassKind, QualityModel};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedPass {
    pub kind: PassKind,
    pub source_layers: usize,
    pub source_bytes: u64,
}

/// Chooses no more than three client-visible updates. The middle pass is the
/// available layer whose cumulative bytes are closest to half of the final
/// usable byte budget; this avoids decoding every possible visual candidate.
pub fn plan_passes(model: &QualityModel) -> Vec<PlannedPass> {
    let Some(&final_bytes) = model.cumulative_layer_bytes.last() else {
        return Vec::new();
    };

    let preview_index = model
        .cumulative_layer_bytes
        .iter()
        .position(|&bytes| bytes > 0)
        .unwrap_or(0);
    let mut indexes = vec![preview_index];

    if model.cumulative_layer_bytes.len() > 2 {
        let target = final_bytes / 2;
        let gain_index = model
            .cumulative_layer_bytes
            .iter()
            .enumerate()
            .min_by_key(|(_, bytes)| bytes.abs_diff(target))
            .map(|(index, _)| index)
            .unwrap_or(preview_index);
        if gain_index != preview_index && gain_index + 1 != model.cumulative_layer_bytes.len() {
            indexes.push(gain_index);
        }
    }

    let final_index = model.cumulative_layer_bytes.len() - 1;
    if indexes.last().copied() != Some(final_index) {
        indexes.push(final_index);
    }

    indexes
        .into_iter()
        .enumerate()
        .map(|(ordinal, index)| PlannedPass {
            kind: match (ordinal, index == final_index) {
                (_, true) => PassKind::Final,
                (0, false) => PassKind::Preview,
                _ => PassKind::Gain,
            },
            source_layers: index + 1,
            source_bytes: model.cumulative_layer_bytes[index],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_preview_gain_and_final_without_duplicate_layer() {
        let passes = plan_passes(&QualityModel {
            cumulative_layer_bytes: vec![120, 520, 1_000],
        });

        assert_eq!(passes.len(), 3);
        assert_eq!(passes[0].kind, PassKind::Preview);
        assert_eq!(passes[1].kind, PassKind::Gain);
        assert_eq!(passes[2].kind, PassKind::Final);
    }
}
