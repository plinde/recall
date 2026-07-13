.PHONY: build install

build:
	cargo build

install: build
	cp target/debug/recall ~/.local/bin/recall
	chmod u+x ~/.local/bin/recall
