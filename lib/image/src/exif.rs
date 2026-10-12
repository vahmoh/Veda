//! The orientation tag of EXIF data.
//!
//! The JPEG decoder hands over a file's EXIF data (the TIFF structure after the `Exif\0\0`
//! header of its APP1 segment); only the orientation is of interest here, which is in the
//! first image file directory.

use crate::Orientation;

/// The orientation recorded in EXIF data, if it has one.
pub(crate) fn orientation(tiff: &[u8]) -> Option<Orientation> {
    let le = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let r16 = |o: usize| -> Option<u16> {
        let b = tiff.get(o..o.checked_add(2)?)?;
        Some(if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    };
    let r32 = |o: usize| -> Option<u32> {
        let b = tiff.get(o..o.checked_add(4)?)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    };
    if r16(2)? != 42 {
        return None;
    }
    let ifd = r32(4)? as usize;
    let count = r16(ifd)? as usize;
    for i in 0..count.min(1024) {
        let e = ifd + 2 + i * 12;
        if r16(e)? == 0x0112 {
            let value = match r16(e + 2)? {
                3 => r16(e + 8)?,
                4 => r32(e + 8)? as u16,
                _ => return None,
            };
            return Orientation::from_exif(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_orientation_in_either_byte_order() {
        let mut le = b"II*\0\x08\0\0\0\x02\0".to_vec();
        le.extend_from_slice(&[0x0F, 0x01, 2, 0, 4, 0, 0, 0, 0x30, 0, 0, 0]); // Make
        le.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]); // Orientation = 6
        assert_eq!(orientation(&le), Some(Orientation::Rotate90));
        let mut be = b"MM\0*\0\0\0\x08\0\x01".to_vec();
        be.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 8, 0, 0]);
        assert_eq!(orientation(&be), Some(Orientation::Rotate270));
        assert_eq!(orientation(&be[..14]), None);
        assert_eq!(orientation(b"XX"), None);
        // An offset to the end of memory is no orientation.
        assert_eq!(orientation(b"II*\0\xFF\xFF\xFF\xFF"), None);
    }
}
