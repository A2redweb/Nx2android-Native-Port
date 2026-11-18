use super::*;

const BID: &str = "d49165c05ead8ae140f189aa97934f39";
const UNITY: &str = "ALL_FPS:\n  # Vsync\n  -\n    type: evaluate_write\n    address: [MAIN, 0x7A642D8]\n    value_type: int32\n    value: VSYNC_TARGET\n  -\n    type: evaluate_write\n    address: [MAIN, 0x7A642E4]\n    value_type: float\n    value: \"1 / FPS_LOCK_TARGET\"\n";

fn rows(text: &str, image: &[u8]) -> (Vec<Vec<String>>, Vec<CutRow>) {
    let (tsv, cuts) = convert(text, image, BID, "test").unwrap();
    (tsv.lines().filter(|l| !l.starts_with('#')).map(|l| l.split('\t').map(String::from).collect()).collect(), cuts)
}

#[test]
fn unity_vsync_and_frame_time() {
    let (r, cuts) = rows(UNITY, b"");
    assert_eq!(r[0], vec!["build", BID]);
    assert!(cuts.is_empty());
    assert_eq!(r[1][1..], ["int32", "0x7a642d8", "2", "1", "-"]);
    assert_eq!(&r[2][1..3], ["float", "0x7a642e4"]);
    assert!((r[2][3].parse::<f64>().unwrap() - 1.0 / 30.0).abs() < 1e-12);
    assert!((r[2][4].parse::<f64>().unwrap() - 1.0 / 62.0).abs() < 1e-12);
}

#[test]
fn expressions() {
    for (e, t, v) in [("FPS_TARGET*2", 30, 60.0), ("1000/FRAMETIME_TARGET", 60, 60.0), ("2^3", 30, 8.0), ("-2^2", 30, -4.0), ("TruncDec(FRAMETIME_TARGET, 2)", 60, 16.66), ("floor(7/2)", 30, 3.0), ("(1+2)*3-4%3", 30, 8.0)] {
        assert!((expr::eval(e, t).unwrap().f() - v).abs() < 1e-9, "{e}");
    }
    for e in ["nope", "1+", "FPS_TARGET)", "1/0", "sin(1)"] {
        assert!(expr::eval(e, 30).is_err(), "{e}");
    }
}

#[test]
fn pointers_lists_and_compares() {
    let (r, _) = rows(
        "ALL_FPS:\n  -\n    type: compare\n    compare_address: [MAIN, 0x10, 0x8]\n    compare_type: \"!=\"\n    compare_value_type: int8\n    compare_value: 0\n    address: [MAIN, 0x20, 0x30, -4]\n    value_type: int32\n    value: [2, 2]\n  -\n    type: block\n    what: timing\n",
        b"",
    );
    assert_eq!(r[1][1..], ["int32", "0x20,0x30,-0x4", "2,2", "2,2", "int8 0x10,0x8 != 0"]);
    assert_eq!(r.len(), 2); // block timing dropped
}

#[test]
fn refused_whole() {
    let decl = format!("{UNITY}DECLARATIONS:\n  -\n    type: variable\n    name: x\n    value_type: float\n    default_value: 0\n");
    let asm = format!("{UNITY}MASTER_WRITE:\n  -\n    type: asm_a64\n    main_offset: 0x10\n    instructions: [[b.ne, :goto1], [nop], :goto1, [nop]]\n");
    for t in [
        decl.as_str(),
        asm.as_str(),
        "ALL_FPS:\n  -\n    type: write\n    address: [HEAP, 0x10]\n    value_type: int32\n    value: 1\n",
        "ALL_FPS:\n  -\n    type: compare\n    compare_address: [MAIN, 0x10]\n    compare_type: \"==\"\n    compare_value_type: int8\n    compare_value: 1\n    value_type: refresh_rate\n    value: 30\n",
        "ALL_FPS: [\n",
        "write: 1\n",
    ] {
        assert!(convert(t, b"", BID, "").is_err(), "{t}");
    }
}

#[test]
fn master_write_bytes_are_cuts() {
    let image: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
    let (_, cuts) = rows(&format!("{UNITY}MASTER_WRITE:\n  -\n    type: bytes\n    main_offset: 0x10\n    value_type: uint8\n    value: [0x1F, 0x20, 0x03]\n"), &image);
    assert_eq!(cuts, vec![CutRow { off: 0x10, old: vec![0x10, 0x11, 0x12], new: vec![0x1f, 0x20, 0x03], why: "FPSLocker-Warehouse MASTER_WRITE entry 1".into() }]);
    assert_eq!(cuts_tsv(BID, &cuts), format!("{BID}\t0x10\t101112\t1f2003\tFPSLocker-Warehouse MASTER_WRITE entry 1\n"));
}

#[test]
fn url_of_a_build() {
    assert!(url("0100fd501f95a000", BID).ends_with("/patches/0100FD501F95A000/D49165C05EAD8AE1.yaml"));
}

#[test]
fn yaml_shapes() {
    let v = yaml::parse("A:\n  - x: 1\n    y: [1, 0x10, \"s\"]\n  - x: 2.5\nB:\n- z\n").unwrap();
    assert_eq!(
        v,
        Value::Map(vec![
            ("A".into(), Value::List(vec![Value::Map(vec![("x".into(), Value::Int(1)), ("y".into(), Value::List(vec![Value::Int(1), Value::Int(16), Value::Str("s".into())]))]), Value::Map(vec![("x".into(), Value::Float(2.5))])])),
            ("B".into(), Value::List(vec![Value::Str("z".into())])),
        ])
    );
}

#[test]
fn a_list_over_several_lines() {
    let v = yaml::parse("ALL_FPS:\n  - type: write\n    value: [\n      1, 2,\n      3\n    ]\n").unwrap();
    assert_eq!(
        v,
        Value::Map(vec![("ALL_FPS".into(), Value::List(vec![Value::Map(vec![("type".into(), Value::Str("write".into())), ("value".into(), Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))])]))])
    );
}
