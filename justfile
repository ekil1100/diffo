set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

binary := "diffo"
install_dir := env_var("HOME") + "/.local/bin"

default:
    just --list

version:
    zig version

target:
    zig targets

targets: target

build:
    zig build

test:
    zig build test

check: test build

release target="native":
    if [[ "{{target}}" == "native" ]]; then \
        zig build --release=fast; \
    else \
        zig build --release=fast -Dtarget="{{target}}"; \
    fi

install target="native": (release target)
    mkdir -p "{{install_dir}}"
    cp "zig-out/bin/{{binary}}" "{{install_dir}}/{{binary}}"
    chmod +x "{{install_dir}}/{{binary}}"
    "{{install_dir}}/{{binary}}" --help >/dev/null
    echo "installed {{binary}} to {{install_dir}}/{{binary}}"
