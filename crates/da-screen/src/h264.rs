//! H.264 bitstream helpers.
//!
//! On the wire every frame is Annex B (start codes before each NAL unit),
//! which is what Media Foundation produces and consumes. VideoToolbox works
//! in AVCC form (a big-endian length before each NAL unit, with SPS/PPS kept
//! in the format description), so the macOS side converts at the edges.

pub const NAL_IDR: u8 = 5;
pub const NAL_SEI: u8 = 6;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1f)
}

/// The NAL units in an Annex B buffer, without their start codes.
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new(); // (start of start code, start of NAL)
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let sc = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((sc, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &(_, nal_start)) in starts.iter().enumerate() {
        let end = starts.get(k + 1).map_or(data.len(), |&(sc, _)| sc);
        if end > nal_start {
            out.push(&data[nal_start..end]);
        }
    }
    out
}

/// True if the access unit holds an IDR slice.
pub fn is_keyframe(annexb: &[u8]) -> bool {
    nal_units(annexb).iter().any(|n| nal_type(n) == NAL_IDR)
}

/// The SPS and PPS in an Annex B access unit, if it carries them.
pub fn parameter_sets(annexb: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let nals = nal_units(annexb);
    let sps = nals.iter().find(|n| nal_type(n) == NAL_SPS)?;
    let pps = nals.iter().find(|n| nal_type(n) == NAL_PPS)?;
    Some((sps.to_vec(), pps.to_vec()))
}

/// Annex B -> AVCC with 4-byte lengths, leaving out parameter sets and
/// access unit delimiters (VideoToolbox takes those from the format
/// description).
pub fn annexb_to_avcc(annexb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(annexb.len() + 16);
    for nal in nal_units(annexb) {
        if matches!(nal_type(nal), NAL_SPS | NAL_PPS | NAL_AUD) {
            continue;
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// AVCC (lengths of `len_size` bytes) -> Annex B, appended to `out`.
pub fn avcc_to_annexb(avcc: &[u8], len_size: usize, out: &mut Vec<u8>) {
    let mut i = 0;
    while i + len_size <= avcc.len() {
        let mut n = 0usize;
        for b in &avcc[i..i + len_size] {
            n = (n << 8) | *b as usize;
        }
        i += len_size;
        let end = (i + n).min(avcc.len());
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&avcc[i..end]);
        i = end;
    }
}

/// Append one NAL unit with a start code.
pub fn push_nal(out: &mut Vec<u8>, nal: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0x67, 0x64, 0x00, 0x28];
    const PPS: &[u8] = &[0x68, 0xee, 0x3c, 0x80];
    const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x00, 0x03, 0x01];
    const P: &[u8] = &[0x41, 0x9a, 0x02];

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for (i, n) in nals.iter().enumerate() {
            // Mix 3- and 4-byte start codes like real encoders do.
            if i % 2 == 0 {
                v.extend_from_slice(&[0, 0, 0, 1]);
            } else {
                v.extend_from_slice(&[0, 0, 1]);
            }
            v.extend_from_slice(n);
        }
        v
    }

    #[test]
    fn splits_nal_units() {
        let data = annexb(&[SPS, PPS, IDR]);
        assert_eq!(nal_units(&data), vec![SPS, PPS, IDR]);
        assert!(is_keyframe(&data));
        assert!(!is_keyframe(&annexb(&[P])));
    }

    #[test]
    fn finds_parameter_sets() {
        let (s, p) = parameter_sets(&annexb(&[SPS, PPS, IDR])).unwrap();
        assert_eq!((s.as_slice(), p.as_slice()), (SPS, PPS));
        assert!(parameter_sets(&annexb(&[P])).is_none());
    }

    #[test]
    fn avcc_roundtrip_drops_parameter_sets() {
        let data = annexb(&[SPS, PPS, IDR, P]);
        let avcc = annexb_to_avcc(&data);
        let mut back = Vec::new();
        avcc_to_annexb(&avcc, 4, &mut back);
        assert_eq!(nal_units(&back), vec![IDR, P]);
    }
}
