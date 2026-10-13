use super::*;

fn writes(source: &str) -> bool {
    let words = crate::tokenize::tokenize(source);
    let program = words[0].basename();
    let args: Vec<&str> = words.iter().skip(1).map(|t| t.text.as_str()).collect();
    match program.as_str() {
        "sed" => sed_writes(&args),
        "awk" => awk_writes(&args),
        _ => writes_through_own_arguments(&program, &words),
    }
}

#[test]
fn output_and_program_options_are_found_in_bundles_and_abbreviations() {
    for source in [
        "sort -uo out in",
        "sort -o out in",
        "sort --out=f in",
        "sort --output f in",
        "sort --compress-program=gzip in",
        "sort --compress=gzip in",
        "uniq in out",
        "uniq -c in out",
        "uniq -f 1 in out",
        "uniq --skip-fields 1 in out",
        "tree -o out",
        "tree -aR",
        "date -s now",
        "date -us now",
        "date --set=now",
        "date 0101000025",
        "file -C -m magic",
        "file --compile",
        "fd -x rm",
        "fd -Hx rm",
        "fd -X rm",
        "fd --exec rm",
        "rg --pre cat x",
        "rg --pre=cat x",
        "printenv",
    ] {
        assert!(writes(source), "{source:?} writes or executes");
    }
    for source in [
        "sort -k1,1 in",
        "sort -t, -k2 in",
        "sort -r in",
        "uniq in",
        "uniq -c in",
        "uniq -f 1 in",
        "uniq -f1 in",
        "tree -L 2",
        "date",
        "date +%s",
        "date -d yesterday +%F",
        "date -u",
        "file -b x",
        "fd -e rs",
        "fd -ex foo",
        "rg --pre-glob '*.pdf' x",
        "printenv HOME",
    ] {
        assert!(!writes(source), "{source:?} only reads");
    }
}

#[test]
fn sed_scripts_that_write_or_execute() {
    for source in [
        "sed -i s/a/b/ f",
        "sed -i.bak s/a/b/ f",
        "sed --in-place s/a/b/ f",
        "sed -f script.sed f",
        "sed 'w out' f",
        "sed 's/a/b/gw out' f",
        "sed 's/a/b/e' f",
        "sed 'e ls' f",
        "sed '1w out' f",
        "sed '/x/w out' f",
        "sed '$W out' f",
        "sed -n -e p -e 'w out' f",
        "sed -l 5 'w out' f",
        "sed --expression='w out' f",
    ] {
        assert!(writes(source), "{source:?}");
    }
    for source in [
        "sed 's/foo/bar/g' f",
        "sed -n '1,5p' f",
        "sed -e s/a/b/ -e s/c/d/ w.txt",
        "sed 's/../x/g' f",
        "sed '/start/,/end/d' f",
        "sed -E 's/(a|b)/x/' f",
        "sed 'y/abc/xyz/' f",
    ] {
        assert!(!writes(source), "{source:?}");
    }
}

#[test]
fn awk_programs_that_write_pipe_or_execute() {
    for source in [
        "awk '{print > \"out\"}' f",
        "awk '{print $1 >> \"out\"}' f",
        "awk '{print > $2}' f",
        "awk '{printf(\"%s\", $1) > FILENAME \".bak\"}' f",
        "awk 'BEGIN{print \"x\" | \"sh\"}'",
        "awk 'BEGIN{\"date\" | getline d}'",
        "awk '{getline line < \"/etc/passwd\"}' f",
        "awk '{system(\"rm x\")}' f",
        "awk '@load \"ext\"'",
        "awk -f prog.awk f",
        "awk -E prog f",
        "awk --profile '{print}' f",
        "awk",
    ] {
        assert!(writes(source), "{source:?}");
    }
    for source in [
        "awk '{print $1}' f",
        "awk -F: '{print $1, $3}' /etc/passwd",
        "awk -F '|' '{print $2}' f",
        "awk -v n=3 '$3 > n {print $1}' f",
        "awk '$3 > 100 {print $1}' f",
        "awk '{if ($3 > 100) print $1}' f",
        "awk '{print ($1 > 5)}' f",
        "awk '/a|b/ {print}' f",
        "awk '{n = split($0, a, \"|\"); print n}' f",
        "awk '{print $1 / 2}' f",
    ] {
        assert!(!writes(source), "{source:?}");
    }
}
