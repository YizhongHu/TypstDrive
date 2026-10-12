// Viewers may request state and advertise awareness, but never submit document
// state. Require exactly one well-formed message, preventing appended updates.
pub fn viewer_packet_allowed(packet: &[u8]) -> bool {
    fn uint(bytes: &[u8], offset: &mut usize) -> Option<usize> {
        let mut value = 0usize;
        for shift in (0..usize::BITS).step_by(7) {
            let byte = *bytes.get(*offset)?;
            *offset += 1;
            let part = (byte & 127) as usize;
            if part > (usize::MAX >> shift) {
                return None;
            }
            value |= part << shift;
            if byte & 128 == 0 {
                return Some(value);
            }
        }
        None
    }
    let mut offset = 0;
    match uint(packet, &mut offset) {
        Some(0) if uint(packet, &mut offset) == Some(0) => (),
        Some(1) => (),
        _ => return false,
    }
    let Some(length) = uint(packet, &mut offset) else { return false; };
    offset.checked_add(length) == Some(packet.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn viewers_can_receive_state_but_cannot_send_updates() {
        assert!(viewer_packet_allowed(&[0, 0, 1, 0]));
        assert!(viewer_packet_allowed(&[1, 1, 0]));
        for packet in [
            &[0, 1, 1, 0][..],
            &[0, 2, 1, 0],
            &[0, 0, 1, 0, 0, 2, 1, 0],
            &[1, 0, 0, 2, 0],
            &[],
            &[0, 0, 128],
            &[0, 0, 2, 0],
            &[127],
        ] {
            assert!(!viewer_packet_allowed(packet), "{packet:?}");
        }
    }
}
