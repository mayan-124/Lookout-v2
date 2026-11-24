#![allow(clippy::unwrap_used, clippy::expect_used)]

use lookout_core::plugins::sdk::Handler;
use lookout_screenshot::handler::ScreenshotPlugin;
use serde_json::json;

#[test]
fn test_json_rpc_conformance() {
    let mut plugin = ScreenshotPlugin::without_hotkey();

    // 1. initialize
    let temp_data = tempfile::tempdir().unwrap();
    let init_params = json!({
        "protocol": 1,
        "data_dir": temp_data.path().to_string_lossy(),
        "config": {
            "hotkey": "Ctrl+Alt+S",
            "save_folder": temp_data.path().join("screenshots").to_string_lossy(),
        },
        "paused": false,
        "key": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
    });

    let init_res = plugin.handle("initialize", init_params).unwrap();
    assert_eq!(init_res["protocol"], 1);

    // 2. query
    let query_params = json!({
        "raw": "shot",
        "verb": "shot",
        "rest": "",
        "candidates": []
    });
    let query_res = plugin.handle("query", query_params).unwrap();
    let items = query_res["items"].as_array().expect("items array");
    assert_eq!(items.len(), 6);
    assert_eq!(items[0]["title"], "Take a screenshot");
    assert_eq!(items[0]["action"], "take_screenshot");
    assert_eq!(items[1]["title"], "Scrolling capture");
    assert_eq!(items[1]["action"], "take_scrolling_screenshot");
    assert_eq!(items[2]["title"], "Record screen (MP4)");
    assert_eq!(items[2]["action"], "record_screen");
    assert_eq!(items[3]["title"], "Record screen (GIF)");
    assert_eq!(items[3]["action"], "record_screen_gif");
    assert_eq!(items[4]["title"], "Extract text (OCR)");
    assert_eq!(items[4]["action"], "extract_text");
    assert_eq!(items[5]["title"], "Open screenshots folder");
    assert_eq!(items[5]["action"], "open_folder");

    // 3. get_actions
    let actions_res1 = plugin
        .handle("get_actions", json!({"items": [items[0].clone()]}))
        .unwrap();
    assert_eq!(actions_res1["actions"][0]["id"], "take_screenshot");
    assert_eq!(actions_res1["actions"][0]["risk"], "safe");

    let actions_res_scroll = plugin
        .handle("get_actions", json!({"items": [items[1].clone()]}))
        .unwrap();
    assert_eq!(
        actions_res_scroll["actions"][0]["id"],
        "take_scrolling_screenshot"
    );
    assert_eq!(actions_res_scroll["actions"][0]["risk"], "safe");

    let actions_res_rec = plugin
        .handle("get_actions", json!({"items": [items[2].clone()]}))
        .unwrap();
    assert_eq!(actions_res_rec["actions"][0]["id"], "record_screen");
    assert_eq!(actions_res_rec["actions"][0]["risk"], "safe");

    let actions_res_rec_gif = plugin
        .handle("get_actions", json!({"items": [items[3].clone()]}))
        .unwrap();
    assert_eq!(actions_res_rec_gif["actions"][0]["id"], "record_screen_gif");
    assert_eq!(actions_res_rec_gif["actions"][0]["risk"], "safe");

    let actions_res_text = plugin
        .handle("get_actions", json!({"items": [items[4].clone()]}))
        .unwrap();
    assert_eq!(actions_res_text["actions"][0]["id"], "extract_text");
    assert_eq!(actions_res_text["actions"][0]["risk"], "safe");

    let actions_res2 = plugin
        .handle("get_actions", json!({"items": [items[5].clone()]}))
        .unwrap();
    assert_eq!(actions_res2["actions"][0]["id"], "open_folder");
    assert_eq!(actions_res2["actions"][0]["risk"], "safe");

    // 4. preview
    let preview_res1 = plugin
        .handle(
            "preview",
            json!({"action_id": "take_screenshot", "items": [items[0].clone()], "params": {}}),
        )
        .unwrap();
    assert_eq!(preview_res1["title"], "Take a screenshot");
    assert_eq!(preview_res1["undoable"], false);

    let preview_res_scroll = plugin
        .handle(
            "preview",
            json!({"action_id": "take_scrolling_screenshot", "items": [items[1].clone()], "params": {}}),
        )
        .unwrap();
    assert_eq!(preview_res_scroll["title"], "Scrolling capture");
    assert_eq!(preview_res_scroll["undoable"], false);

    let preview_res_rec = plugin
        .handle(
            "preview",
            json!({"action_id": "record_screen", "items": [items[2].clone()], "params": {}}),
        )
        .unwrap();
    assert_eq!(preview_res_rec["title"], "Record screen (MP4)");
    assert_eq!(preview_res_rec["undoable"], false);

    let preview_res_rec_gif = plugin
        .handle(
            "preview",
            json!({"action_id": "record_screen_gif", "items": [items[3].clone()], "params": {}}),
        )
        .unwrap();
    assert_eq!(preview_res_rec_gif["title"], "Record screen (GIF)");
    assert_eq!(preview_res_rec_gif["undoable"], false);

    let preview_res_text = plugin
        .handle(
            "preview",
            json!({"action_id": "extract_text", "items": [items[4].clone()], "params": {}}),
        )
        .unwrap();
    assert_eq!(preview_res_text["title"], "Extract text (OCR)");
    assert_eq!(preview_res_text["undoable"], false);

    let preview_res2 = plugin
        .handle(
            "preview",
            json!({"action_id": "open_folder", "items": [items[5].clone()], "params": {}}),
        )
        .unwrap();
    assert_eq!(preview_res2["title"], "Open screenshots folder");

    // 5. execute
    let exec_res1 = plugin
        .handle(
            "execute",
            json!({"action_id": "take_screenshot", "items": [items[0].clone()], "params": {}}),
        )
        .unwrap();
    assert!(exec_res1["message"].as_str().unwrap().contains("started"));

    let exec_res_scroll = plugin
        .handle(
            "execute",
            json!({"action_id": "take_scrolling_screenshot", "items": [items[1].clone()], "params": {}}),
        )
        .unwrap();
    assert!(exec_res_scroll["message"]
        .as_str()
        .unwrap()
        .contains("started"));

    let exec_res_rec = plugin
        .handle(
            "execute",
            json!({"action_id": "record_screen", "items": [items[2].clone()], "params": {}}),
        )
        .unwrap();
    assert!(exec_res_rec["message"]
        .as_str()
        .unwrap()
        .contains("started"));

    let exec_res_rec_gif = plugin
        .handle(
            "execute",
            json!({"action_id": "record_screen_gif", "items": [items[3].clone()], "params": {}}),
        )
        .unwrap();
    assert!(exec_res_rec_gif["message"]
        .as_str()
        .unwrap()
        .contains("started"));

    let exec_res_text = plugin
        .handle(
            "execute",
            json!({"action_id": "extract_text", "items": [items[4].clone()], "params": {}}),
        )
        .unwrap();
    assert!(exec_res_text["message"]
        .as_str()
        .unwrap()
        .contains("started"));

    // 5b. unknown action ids must return an error and not start capture
    let unknown_exec_res = plugin.handle(
        "execute",
        json!({"action_id": "nonexistent_action", "items": [items[0].clone()], "params": {}}),
    );
    assert!(unknown_exec_res.is_err());
    assert!(unknown_exec_res.unwrap_err().contains("unknown action id"));

    let unknown_preview_res = plugin.handle(
        "preview",
        json!({"action_id": "nonexistent_action", "items": [items[0].clone()], "params": {}}),
    );
    assert!(unknown_preview_res.is_err());

    // 6. undo
    let undo_res = plugin
        .handle("undo", json!({"undo_token": "abc", "dry_run": false}))
        .unwrap();
    assert!(undo_res["message"].as_str().unwrap().contains("undo"));

    // 7. pause_changed notification
    plugin.notify("pause_changed", json!({"paused": true}));

    // 8. shutdown
    let shutdown_res = plugin.handle("shutdown", json!({})).unwrap();
    assert!(shutdown_res.is_null());
}

#[test]
fn test_invalid_hotkey_config_fallback() {
    let mut plugin = ScreenshotPlugin::without_hotkey();
    let temp_data = tempfile::tempdir().unwrap();

    let init_params = json!({
        "protocol": 1,
        "data_dir": temp_data.path().to_string_lossy(),
        "config": {
            "hotkey": "InvalidNoModifierKey",
            "save_folder": temp_data.path().join("screenshots").to_string_lossy(),
        },
        "paused": false,
        "key": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
    });

    // Should succeed and fall back to default "Ctrl+Alt+S"
    let res = plugin.handle("initialize", init_params).unwrap();
    assert_eq!(res["protocol"], 1);

    // Verify query shows the default hotkey
    let q = plugin
        .handle(
            "query",
            json!({"raw": "shot", "verb": "shot", "rest": "", "candidates": []}),
        )
        .unwrap();
    let items = q["items"].as_array().unwrap();
    let sub = items[0]["subtitle"].as_str().unwrap();
    assert!(sub.contains("Ctrl+Alt+S"));
}
