//! Extra scientific bands are not alpha unless JP2 explicitly says so.
//! Default display uses components0,1,2 in codestream order, not a scientific
//! true-colour/band-selection claim; original bands and entropy are unchanged.
pub fn rgb_only(components: usize, container: &[u8]) -> bool {
    if components > 4 {
        return true;
    }
    if components != 4 {
        return false;
    }
    fn alpha(data: &[u8], depth: usize) -> bool {
        if depth > 4 {
            return false;
        }
        let mut pos = 0;
        while pos + 8 <= data.len() {
            let n = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            let kind = &data[pos + 4..pos + 8];
            if n < 8 || pos.checked_add(n).is_none_or(|end| end > data.len()) {
                break;
            }
            let payload = &data[pos + 8..pos + n];
            if kind == b"jp2h" && alpha(payload, depth + 1) {
                return true;
            }
            if kind == b"cdef" && payload.len() >= 2 {
                let count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
                for entry in payload[2..].chunks_exact(6).take(count) {
                    let channel = u16::from_be_bytes([entry[0], entry[1]]);
                    let role = u16::from_be_bytes([entry[2], entry[3]]);
                    if channel == 3 && matches!(role, 1 | 2) {
                        return true;
                    }
                }
            }
            pos += n;
        }
        false
    }
    !alpha(container, 0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multispectral_extras_are_not_opacity() {
        assert!(rgb_only(4, &[]));
        assert!(rgb_only(5, &[]));
        assert!(!rgb_only(3, &[]));
        let mut cdef = vec![];
        cdef.extend_from_slice(&16u32.to_be_bytes());
        cdef.extend_from_slice(b"cdef");
        cdef.extend_from_slice(&[0, 1, 0, 3, 0, 1, 0, 0]);
        assert!(!rgb_only(4, &cdef));
        cdef[13] = 0;
        assert!(rgb_only(4, &cdef));
    }
}
