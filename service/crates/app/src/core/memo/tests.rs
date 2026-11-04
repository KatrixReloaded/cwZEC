use super::*;

fn padded(text: &str) -> [u8; 512] {
    let mut bytes = [0; 512];
    bytes[..text.len()].copy_from_slice(text.as_bytes());
    bytes
}

#[test]
fn sender_selects_recipient_and_noncanonical_memos_are_rejected() {
    let a = "0x0123456789abcdef0123456789abcdef01234567";
    let b = "0x1111111111111111111111111111111111111111";
    let text = make_memo(a).unwrap();
    assert_eq!(make_memo(&a.to_ascii_uppercase()).unwrap(), text);
    assert_eq!(hex::encode(parse_memo(&padded(&text)).unwrap()), &a[2..]);
    assert_ne!(
        parse_memo(&padded(&text)).unwrap(),
        parse_memo(&padded(&make_memo(b).unwrap())).unwrap()
    );
    for invalid in [
        text.replace("11155111", "1"),
        text.replace("abcdef", "ABCDEF"),
        format!("{text} extra"),
        format!("{text}\0extra"),
        text.replace(a, b),
        text.replace("cwzec", "cwzéc"),
    ] {
        assert!(parse_memo(&padded(&invalid)).is_err());
    }
    assert!(parse_memo(&[0; 512]).is_err());
    let mut no_memo = [0; 512];
    no_memo[0] = 0xf6;
    assert!(parse_memo(&no_memo).is_err());
}

#[test]
fn ironwood_tag_matches_independent_public_vector() {
    let mut rho = [0; 32];
    let mut rseed = [0; 32];
    hex::decode_to_slice(
        "2cb5b406ed8985e18130ab33362697b0e4e4c763ccb8f676495c222f7fba1e31",
        &mut rho,
    )
    .unwrap();
    hex::decode_to_slice(
        "defa3d5a57efc2e1e9b01a035587d5fb1a38e01d94903d3c3e0ad3360c1d3710",
        &mut rseed,
    )
    .unwrap();
    assert_eq!(
        hex::encode(deposit_id(&rho, &rseed)),
        "66fee47de74062b5f8b5d2ee291d8726558301999ee18e8c1695bb6c7c0ab514"
    );
}
