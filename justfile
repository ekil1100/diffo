set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

binary := "diffo"
install_dir := env_var("HOME") + "/.local/bin"

default:
    just --list

version:
    rustc --version
    cargo --version

target:
    rustc --print target-list

targets: target

build:
    cargo build --locked

test:
    cargo test --locked

fmt:
    cargo fmt --all

check:
    cargo fmt --all -- --check
    cargo clippy --locked --all-targets -- -D warnings
    cargo test --locked
    cargo build --locked

test-terminal: build
    @test_binary="$(cargo test --locked --lib --no-run --message-format=json | python3 -c 'import json, sys; artifacts = [json.loads(line) for line in sys.stdin]; print(next(a["executable"] for a in artifacts if a.get("executable") and a.get("target", {}).get("kind") == ["lib"]))')"; \
        printf 'python3 tests/terminal.py --test-binary %q\n' "$test_binary"; \
        python3 tests/terminal.py --test-binary "$test_binary"

release target="native":
    cargo build --release --locked{{ if target == "native" { "" } else { " --target " + quote(target) } }}

@install: release
    mkdir -p "{{ install_dir }}"
    cp "target/release/{{ binary }}" "{{ install_dir }}/{{ binary }}"
    chmod +x "{{ install_dir }}/{{ binary }}"
    "{{ install_dir }}/{{ binary }}" --help >/dev/null
    echo "installed {{ binary }} to {{ install_dir }}/{{ binary }}"
