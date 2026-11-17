CARGO ?= cargo
NPM ?= npm
UI_DIR := ui
CAPTURES_DIR := captures

.PHONY: all ui build test demo-868 demo-433 gen-samples clean

all: build

ui:
	cd $(UI_DIR) && $(NPM) install &&$(NPM) run build

build: ui
	$(CARGO) build --release

test:
	$(CARGO) test

demo-868: build
	$(CARGO) run --release -- monitor --source sim --band 868 --bind 127.0.0.1:8080

demo-433: build
	$(CARGO) run --release -- monitor --source sim --band 433 --bind 127.0.0.1:8080

gen-samples:
	mkdir -p $(CAPTURES_DIR)
	$(CARGO) run --release -- generate-sigmf --output $(CAPTURES_DIR)/eu868_clean --duration-sec 8 --band 868 --scenario clean
	$(CARGO) run --release -- generate-sigmf --output $(CAPTURES_DIR)/eu868_jamming --duration-sec 8 --band 868 --scenario jamming
	$(CARGO) run --release -- generate-sigmf --output $(CAPTURES_DIR)/eu868_replay --duration-sec 8 --band 868 --scenario replay
	$(CARGO) run --release -- generate-sigmf --output $(CAPTURES_DIR)/eu868_rogue --duration-sec 8 --band 868 --scenario rogue

clean:
	$(CARGO) clean
	rm -rf $(UI_DIR)/dist $(CAPTURES_DIR)/*.sigmf-data$(CAPTURES_DIR)/*.sigmf-meta