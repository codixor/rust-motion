//! Regression tests for the public `SceneNode::AnimatedStroke` boxing change.
//!
//! `stroke` changed from `rust_motion_paint::Stroke` to
//! `Box<rust_motion_paint::Stroke>` to shrink `SceneNode` (clippy
//! `large_enum_variant` remediation). serde treats `Box<T>` transparently,
//! so the serialized JSON representation is unchanged; the fixtures pin the
//! exact pre-box JSON bytes and these tests prove the post-box serializer
//! still emits them.

use rust_motion_rasterizer::paint::{BrushSettings, Stroke, StrokePoint};
use rust_motion_rasterizer::{Color, Scene, SceneNode};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(path).expect("read fixture")
}

fn make_node() -> SceneNode {
    let brush = BrushSettings {
        size: 8.0,
        ..Default::default()
    };
    let stroke = Stroke {
        brush,
        points: vec![
            StrokePoint::new(10.0, 20.0, 0.4),
            StrokePoint::new(30.0, 40.0, 0.9),
        ],
    };
    SceneNode::AnimatedStroke {
        stroke: Box::new(stroke),
        progress: 0.65,
        color: Some(Color::rgb(255, 128, 0)),
        opacity: 0.8,
    }
}

/// Serializing the boxed variant must be byte-identical to the JSON the
/// pre-box variant produced (fixture captured before the boxing change).
#[test]
fn animated_stroke_box_serializes_like_pre_box_fixture() {
    let node = make_node();
    let json = serde_json::to_string(&node).expect("serialize");
    assert_eq!(
        json,
        fixture("scene_node_animated_stroke_before_box.json"),
        "boxing must not change the serialized JSON representation"
    );
}

/// Reading the pre-box fixture through the boxed field must yield the same
/// construction values.
#[test]
fn animated_stroke_box_reads_pre_box_fixture() {
    let json = fixture("scene_node_animated_stroke_before_box.json");
    let node: SceneNode = serde_json::from_str(&json).expect("deserialize");
    match node {
        SceneNode::AnimatedStroke {
            stroke,
            progress,
            color,
            opacity,
        } => {
            assert!((stroke.brush.size - 8.0).abs() < 1e-4);
            assert_eq!(stroke.points.len(), 2);
            assert_eq!(stroke.points[0].x, 10.0);
            assert_eq!(stroke.points[0].y, 20.0);
            assert!((stroke.points[0].pressure - 0.4).abs() < 1e-4);
            assert_eq!(stroke.points[1].x, 30.0);
            assert_eq!(stroke.points[1].y, 40.0);
            assert!((stroke.points[1].pressure - 0.9).abs() < 1e-4);
            assert!((progress - 0.65).abs() < 1e-4);
            assert_eq!(color, Some(Color::rgb(255, 128, 0)));
            assert!((opacity - 0.8).abs() < 1e-4);
        }
        other => panic!("expected AnimatedStroke, got {other:?}"),
    }
}

/// A whole `Scene` containing a boxed AnimatedStroke must serialize exactly
/// like the pre-box scene fixture.
#[test]
fn scene_with_boxed_animated_stroke_serializes_like_pre_box_fixture() {
    let brush = BrushSettings {
        size: 12.0,
        ..Default::default()
    };
    let stroke = Stroke {
        brush,
        points: vec![StrokePoint::new(5.0, 6.0, 1.0)],
    };
    let scene = Scene {
        nodes: vec![SceneNode::AnimatedStroke {
            stroke: Box::new(stroke),
            progress: 0.5,
            color: None,
            opacity: 1.0,
        }],
    };
    let json = serde_json::to_string(&scene).expect("serialize");
    assert_eq!(
        json,
        fixture("scene_with_animated_stroke_before_box.json"),
        "boxing must not change the serialized JSON representation"
    );
}

/// Constructing the boxed variant and round-tripping through JSON must
/// preserve value equality.
#[test]
fn animated_stroke_box_serde_roundtrip() {
    let node = make_node();
    let json = serde_json::to_string(&node).expect("serialize");
    let back: SceneNode = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(node, back);
}
