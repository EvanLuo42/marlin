use marlin_diag::{Level, LineSink, Record, RingSink, Sink};

fn lines<const N: usize>(ring: &RingSink<N>) -> Vec<String> {
    let mut out = Vec::new();
    assert!(ring.for_each_line(|line| out.push(line.to_owned())));
    out
}

fn copied<const N: usize>(ring: &RingSink<N>, room: usize) -> String {
    let mut buf = vec![0u8; room];
    let len = ring.copy_to(&mut buf);
    String::from_utf8(buf[..len].to_vec()).unwrap()
}

#[test]
fn keeps_everything_until_full() {
    let ring = RingSink::<64>::new();
    assert!(lines(&ring).is_empty());
    assert_eq!(copied(&ring, 64), "");
    ring.write_line(Level::Info, "a\n");
    ring.write_line(Level::Info, "bb\n");
    assert_eq!(lines(&ring), ["a", "bb"]);
    assert_eq!(copied(&ring, 64), "a\nbb\n");
    assert_eq!(ring.bytes_written(), 5);
}

#[test]
fn wraps_around_keeping_whole_lines_in_order() {
    let ring = RingSink::<33>::new();
    for i in 0..20 {
        ring.write_line(Level::Info, &format!("line {i:02}\n"));
    }
    // 8 bytes per line; 33 bytes show the last four exactly.
    assert_eq!(lines(&ring), ["line 16", "line 17", "line 18", "line 19"]);

    // Off the line grid, the partly overwritten oldest line is skipped.
    ring.write_line(Level::Info, "xyz\n");
    assert_eq!(lines(&ring), ["line 17", "line 18", "line 19", "xyz"]);
    assert_eq!(copied(&ring, 32), "line 17\nline 18\nline 19\nxyz\n");

    // A smaller buffer gets the newest whole lines that fit.
    assert_eq!(copied(&ring, 15), "line 19\nxyz\n");
    assert_eq!(copied(&ring, 4), "xyz\n");
    assert_eq!(copied(&ring, 3), "");

    ring.clear();
    assert!(lines(&ring).is_empty());
}

#[test]
fn text_without_a_final_newline_and_oversized_writes() {
    let ring = RingSink::<16>::new();
    ring.write("one\ntwo");
    assert_eq!(lines(&ring), ["one", "two"]);
    // Only the tail of a write longer than the ring is kept.
    ring.write(&format!("{}\nend\n", "z".repeat(40)));
    assert_eq!(lines(&ring), ["end"]);
}

#[test]
fn logs_records_and_survives_threads() {
    static RING: RingSink<4096> = RingSink::new();
    std::thread::scope(|scope| {
        for t in 0..4 {
            scope.spawn(move || {
                for i in 0..200 {
                    RING.log(&Record::new(
                        Level::Warn,
                        "ring",
                        file!(),
                        line!(),
                        t,
                        format_args!("{t} {i}"),
                    ));
                }
            });
        }
        scope.spawn(|| {
            for _ in 0..200 {
                let mut buf = [0u8; 512];
                let len = RING.copy_to(&mut buf);
                assert!(std::str::from_utf8(&buf[..len]).is_ok());
            }
        });
    });
    let all = lines(&RING);
    assert!(!all.is_empty());
    // The newest line is some thread's last, and per thread the kept lines are in order.
    assert!(all.last().unwrap().ends_with(" 199"), "{all:?}");
    for t in 0..4u64 {
        let mine: Vec<u32> = all
            .iter()
            .filter(|line| line.contains(&format!("[{t}] ring: ")))
            .map(|line| line.rsplit(' ').next().unwrap().parse().unwrap())
            .collect();
        assert!(mine.windows(2).all(|w| w[0] < w[1]), "{mine:?}");
    }
}
