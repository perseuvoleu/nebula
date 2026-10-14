//! Protocol-29 fixtures from findl 3c6621e; fields use positional MessagePack.
use nebula_core::ClientRequest;

#[test]
fn protocol29_requests_preserve_findl_wire_fields() {
    let fixtures: &[(&[u8], &str)] = &[
        (&[129, 171, 67, 114, 101, 97, 116, 101, 65, 103, 101, 110, 116, 153, 7, 161, 119, 167, 102, 101, 97, 116, 117, 114, 101, 166, 119, 111, 114, 107, 101, 114, 165, 99, 111, 100, 101, 120, 165, 109, 111, 100, 101, 108, 164, 104, 105, 103, 104, 195, 164, 116, 97, 115, 107], r#"{"CreateAgent":{"req_id":7,"worktree":"w","expected_branch":"feature","name":"worker","kind":"codex","model":"model","effort":"high","auto_title":true,"prompt":"task"}}"#),
        (&[129, 177, 73, 110, 112, 117, 116, 65, 99, 107, 110, 111, 119, 108, 101, 100, 103, 101, 100, 148, 7, 161, 97, 161, 112, 196, 3, 27, 91, 65], r#"{"InputAcknowledged":{"req_id":7,"id":"a","project":"p","data":[27,91,65]}}"#),
        (&[129, 180, 67, 104, 101, 99, 107, 87, 111, 114, 107, 116, 114, 101, 101, 82, 101, 109, 111, 118, 97, 108, 149, 7, 161, 119, 167, 102, 101, 97, 116, 117, 114, 101, 168, 47, 102, 105, 120, 116, 117, 114, 101, 194], r#"{"CheckWorktreeRemoval":{"req_id":7,"id":"w","branch":"feature","path":"/fixture","delete":false}}"#),
        (&[129, 177, 68, 101, 108, 101, 116, 101, 65, 103, 101, 110, 116, 83, 99, 111, 112, 101, 100, 147, 7, 161, 97, 161, 112], r#"{"DeleteAgentScoped":{"req_id":7,"id":"a","project":"p"}}"#),
        (&[129, 181, 82, 101, 115, 111, 108, 118, 101, 66, 114, 97, 110, 99, 104, 67, 104, 101, 99, 107, 111, 117, 116, 147, 7, 161, 112, 167, 102, 101, 97, 116, 117, 114, 101], r#"{"ResolveBranchCheckout":{"req_id":7,"project":"p","branch":"feature"}}"#),
        (&[129, 174, 67, 114, 101, 97, 116, 101, 84, 101, 114, 109, 105, 110, 97, 108, 148, 7, 161, 119, 167, 102, 101, 97, 116, 117, 114, 101, 165, 115, 104, 101, 108, 108], r#"{"CreateTerminal":{"req_id":7,"worktree":"w","expected_branch":"feature","name":"shell"}}"#),
    ];
    for (wire, json) in fixtures {
        let request: ClientRequest = rmp_serde::from_slice(wire).expect(json);
        assert_eq!(serde_json::to_value(&request).unwrap(), serde_json::from_str::<serde_json::Value>(json).unwrap());
        assert_eq!(rmp_serde::to_vec(&request).unwrap(), *wire);
    }
}
