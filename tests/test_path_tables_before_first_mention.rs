//! A column-aware trace's `paths.dat` gives every file the table that
//! describes it.
//!
//! In a column-aware trace a file's per-line table is fixed when the file
//! is first mentioned -- by an explicit registration, or by a step, a
//! function, a call or an id request naming it -- and a table offered
//! after that is not honoured (`codetracer-trace-format-spec`,
//! `internal-files.md` §"`paths.dat` Layout A"). So for every file the
//! recorder can read, its real table must reach the writer before the
//! file's first mention; a file it cannot read as source (the program
//! blob itself, or a DWARF path missing from the recording host) carries
//! the conventional table, `100000` lines of `1024` positions, and must
//! never be offered another one later.
//!
//! Each test records a real PolkaVM blob through the real recorder and the
//! real CTFS writer, then reads `paths.dat` back with the Rust interning
//! tables reader. No mocks.
//!
//! The blobs carry a hand-rolled debug line program that covers only some
//! of their instructions, so one recording steps through a source file
//! (DWARF hit) and through the blob path (DWARF miss) -- the fallback
//! that addresses a step by `pc + 1` on the blob.

use std::path::{Path, PathBuf};

use codetracer_trace_reader::interning_tables_reader::open_interning_tables;
use polkavm_common::program::{
    Instruction, InstructionSetKind, LineProgramOp, Reg::*, SECTION_OPT_DEBUG_LINE_PROGRAM_RANGES,
    SECTION_OPT_DEBUG_LINE_PROGRAMS, SECTION_OPT_DEBUG_STRINGS, VERSION_DEBUG_LINE_PROGRAM_V1, asm,
};
use polkavm_common::utils::ArcBytes;
use polkavm_common::varint::{MAX_VARINT_LENGTH, write_varint};
use polkavm_common::writer::ProgramBlobBuilder;

/// The table a file the recorder cannot read as source is recorded with.
const CONVENTIONAL_LINES: usize = 100_000;
const CONVENTIONAL_LINE_LENGTH: u32 = 1024;

fn push_varint(out: &mut Vec<u8>, value: u32) {
    let mut buf = [0u8; MAX_VARINT_LENGTH];
    let n = write_varint(value, &mut buf);
    out.extend_from_slice(&buf[..n]);
}

fn append_debug_string(buf: &mut Vec<u8>, s: &str) -> u32 {
    let offset = buf.len() as u32;
    push_varint(buf, s.len() as u32);
    buf.extend_from_slice(s.as_bytes());
    offset
}

fn code() -> Vec<Instruction> {
    vec![
        asm::load_imm(A0, 1),
        asm::load_imm(A1, 2),
        asm::load_imm(A2, 3),
        asm::ret(),
    ]
}

fn builder_with_code(code: &[Instruction]) -> ProgramBlobBuilder {
    let mut builder = ProgramBlobBuilder::new(InstructionSetKind::Latest32);
    builder.set_stack_size(4096);
    builder.set_rw_data_size(64);
    builder.add_export_by_basic_block(0, b"main");
    builder.set_code(code, &[]);
    builder
}

/// A blob whose debug line program maps the first `lines.len()`
/// instructions to `source_path` at the given lines and leaves the rest
/// unmapped, so the recorder steps through both the source file and the
/// blob-path fallback.
fn build_partially_mapped_blob(source_path: &str, lines: &[u32]) -> Vec<u8> {
    let code = code();
    assert!(
        lines.len() < code.len(),
        "at least one instruction must stay unmapped"
    );
    let bare = builder_with_code(&code)
        .into_vec()
        .expect("bare blob build");
    let parsed =
        polkavm_common::program::ProgramBlob::parse(ArcBytes::from(bare)).expect("parse bare blob");
    let lengths: Vec<u32> = parsed
        .instructions()
        .map(|i| i.next_offset.0 - i.offset.0)
        .collect();

    let mut strings: Vec<u8> = Vec::new();
    let ns_off = append_debug_string(&mut strings, "");
    let fn_off = append_debug_string(&mut strings, "main");
    let path_off = append_debug_string(&mut strings, source_path);

    let mut prog: Vec<u8> = vec![VERSION_DEBUG_LINE_PROGRAM_V1];
    let info_offset: u32 = 1;
    prog.push(LineProgramOp::SetStackDepth as u8);
    push_varint(&mut prog, 1);
    prog.push(LineProgramOp::SetMutationDepth as u8);
    push_varint(&mut prog, 0);
    prog.push(LineProgramOp::SetKindLine as u8);
    prog.push(LineProgramOp::SetNamespace as u8);
    push_varint(&mut prog, ns_off);
    prog.push(LineProgramOp::SetFunctionName as u8);
    push_varint(&mut prog, fn_off);
    prog.push(LineProgramOp::SetPath as u8);
    push_varint(&mut prog, path_off);
    for (idx, &line) in lines.iter().enumerate() {
        prog.push(LineProgramOp::SetLine as u8);
        push_varint(&mut prog, line);
        prog.push(LineProgramOp::FinishMultipleInstructions as u8);
        push_varint(&mut prog, lengths[idx]);
    }
    prog.push(LineProgramOp::FinishProgram as u8);

    let mapped_end: u32 = lengths.iter().take(lines.len()).sum();
    let mut ranges: Vec<u8> = Vec::new();
    ranges.extend_from_slice(&0u32.to_le_bytes());
    ranges.extend_from_slice(&mapped_end.to_le_bytes());
    ranges.extend_from_slice(&info_offset.to_le_bytes());

    let mut builder = builder_with_code(&code);
    builder.add_custom_section(SECTION_OPT_DEBUG_STRINGS, strings);
    builder.add_custom_section(SECTION_OPT_DEBUG_LINE_PROGRAMS, prog);
    builder.add_custom_section(SECTION_OPT_DEBUG_LINE_PROGRAM_RANGES, ranges);
    builder.into_vec().expect("partially mapped blob build")
}

/// Record `blob` (written to `blob_path`) and return the `.ct` container.
fn record(blob_path: &Path, blob: &[u8], out_dir: &Path) -> PathBuf {
    std::fs::write(blob_path, blob).expect("write blob");
    std::fs::create_dir_all(out_dir).unwrap();
    codetracer_polkavm_recorder::recorder::record(blob_path, out_dir)
        .expect("recorder::record should succeed");
    std::fs::read_dir(out_dir)
        .expect("read out_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|ext| ext == "ct"))
        .unwrap_or_else(|| panic!("expected a .ct container in {}", out_dir.display()))
}

/// Every `paths.dat` record of the container, as `(path, table)`.
fn path_tables(ct: &Path) -> Vec<(String, Vec<u32>)> {
    let tables = open_interning_tables(ct)
        .expect("open interning tables")
        .expect("container carries interning tables");
    assert!(tables.is_column_aware(), "the trace must be column-aware");
    (0..tables.path_count() as u64)
        .map(|id| {
            (
                tables.path_str(id).expect("path record"),
                tables.path_line_lengths(id).expect("Layout A record"),
            )
        })
        .collect()
}

/// The source file's real table: the byte length of each line, without
/// its line terminator.
fn real_table(source: &str) -> Vec<u32> {
    source.lines().map(|l| l.len() as u32).collect()
}

fn describe(table: &[u32]) -> String {
    if table.len() > 8 {
        format!(
            "{} lines, first {:?}, last {:?}",
            table.len(),
            &table[..4],
            &table[table.len() - 4..]
        )
    } else {
        format!("{table:?}")
    }
}

fn assert_conventional(path: &str, table: &[u32], why: &str) {
    assert!(
        table.len() == CONVENTIONAL_LINES && table.iter().all(|&l| l == CONVENTIONAL_LINE_LENGTH),
        "{path} ({why}) must carry the conventional table \
         ({CONVENTIONAL_LINES} lines of {CONVENTIONAL_LINE_LENGTH}); it carries {}",
        describe(table)
    );
}

/// One recording steps through a readable source file and then, past the
/// mapped instructions, through the blob path. The source file carries its
/// real table; the blob, which is not source, carries the conventional one.
#[test]
fn source_and_blob_fallback_each_carry_their_table() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src_path = tmp.path().join("partially_mapped.rs");
    let source = "fn main() {\n    let a = 1;\n    let bb = 2;\n}\n";
    std::fs::write(&src_path, source).expect("write source");
    let blob_path = tmp.path().join("partially_mapped.polkavm");
    let blob = build_partially_mapped_blob(&src_path.to_string_lossy(), &[2, 3]);
    let ct = record(&blob_path, &blob, &tmp.path().join("traces"));

    let records = path_tables(&ct);
    let src_str = src_path.to_string_lossy().into_owned();
    let blob_str = blob_path.to_string_lossy().into_owned();
    let names: Vec<&str> = records.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        names.iter().filter(|p| **p == src_str).count(),
        1,
        "the source file must have exactly one paths.dat record; records: {names:?}"
    );
    assert_eq!(
        names.iter().filter(|p| **p == blob_str).count(),
        1,
        "the blob must have exactly one paths.dat record; records: {names:?}"
    );
    for (path, table) in &records {
        if *path == src_str {
            assert_eq!(
                *table,
                real_table(source),
                "{path} must carry its real per-line table; it carries {}",
                describe(table)
            );
        } else if *path == blob_str {
            assert_conventional(path, table, "the program blob, not source");
        } else {
            panic!("unexpected paths.dat record {path:?}; records: {names:?}");
        }
    }
}

/// A blob with no debug information steps only through the blob path,
/// which carries the conventional table and never the newline-split bytes
/// of the binary.
#[test]
fn blob_without_debug_info_carries_the_conventional_table() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let blob_path = tmp.path().join("no_debug_info.polkavm");
    let blob = builder_with_code(&code()).into_vec().expect("blob build");
    let ct = record(&blob_path, &blob, &tmp.path().join("traces"));

    let records = path_tables(&ct);
    assert_eq!(
        records.len(),
        1,
        "only the blob is mentioned; records: {:?}",
        records.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    let (path, table) = &records[0];
    assert_eq!(*path, blob_path.to_string_lossy());
    assert_conventional(path, table, "the program blob, not source");
}

/// A DWARF source path that is not on the recording host cannot be read,
/// so it carries the conventional table.
#[test]
fn unreadable_source_carries_the_conventional_table() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("not_on_this_host.rs");
    let blob_path = tmp.path().join("missing_source.polkavm");
    let blob = build_partially_mapped_blob(&missing.to_string_lossy(), &[7]);
    let ct = record(&blob_path, &blob, &tmp.path().join("traces"));

    let records = path_tables(&ct);
    let missing_str = missing.to_string_lossy().into_owned();
    let record = records
        .iter()
        .find(|(p, _)| *p == missing_str)
        .unwrap_or_else(|| panic!("no paths.dat record for {missing_str}"));
    assert_conventional(&record.0, &record.1, "source not on this host");
    for (path, table) in &records {
        assert!(
            !path.is_empty(),
            "no paths.dat record may have an empty path"
        );
        assert!(
            !table.is_empty() && table.iter().any(|&l| l > 0),
            "{path} carries a table with no positions: {}",
            describe(table)
        );
    }
}

/// A debug line program whose path is empty names no file, so the
/// instruction it maps falls back to the blob path like an unmapped one;
/// no `paths.dat` record has an empty path.
#[test]
fn empty_debug_path_falls_back_to_the_blob() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let blob_path = tmp.path().join("empty_debug_path.polkavm");
    let blob = build_partially_mapped_blob("", &[3]);
    let ct = record(&blob_path, &blob, &tmp.path().join("traces"));

    let records = path_tables(&ct);
    let names: Vec<&str> = records.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        names,
        vec![blob_path.to_string_lossy().as_ref()],
        "only the blob may be recorded; records: {names:?}"
    );
    assert_conventional(&records[0].0, &records[0].1, "the program blob, not source");
}

/// A source file whose lines hold nothing still has a non-zero size: its
/// first line gets one position.
#[test]
fn empty_source_file_gets_one_position() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src_path = tmp.path().join("empty.rs");
    std::fs::write(&src_path, "").expect("write source");
    let blob_path = tmp.path().join("empty_source.polkavm");
    let blob = build_partially_mapped_blob(&src_path.to_string_lossy(), &[1]);
    let ct = record(&blob_path, &blob, &tmp.path().join("traces"));

    let records = path_tables(&ct);
    let src_str = src_path.to_string_lossy().into_owned();
    let (_, table) = records
        .iter()
        .find(|(p, _)| *p == src_str)
        .unwrap_or_else(|| panic!("no paths.dat record for {src_str}"));
    assert_eq!(*table, vec![1], "an empty file's table is [1]");
}
