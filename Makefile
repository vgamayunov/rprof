CARGO ?= cargo
CC ?= cc
MPICC ?= mpicc
PROFILE ?= debug
CARGO_FLAGS = $(if $(filter release,$(PROFILE)),--release,)
PRELOAD = target/$(PROFILE)/librprof_io.so

.PHONY: build preload mpi ompt test

build:
	$(CARGO) build --workspace $(CARGO_FLAGS)
	$(MAKE) preload PROFILE=$(PROFILE)

# Link the C interposition surface around the Rust static library. A Rust cdylib
# would hide C variadic symbols with its generated export map.
preload:
	$(CARGO) build -p rprof-preload $(CARGO_FLAGS)
	@set -eu; temporary=$$(mktemp "$(PRELOAD).XXXXXX"); \
		trap 'rm -f "$$temporary"' EXIT; \
		$(CC) -std=c11 -O2 -g -fPIC -shared -Wall -Wextra -Werror \
		crates/rprof-preload/native/wrappers.c target/$(PROFILE)/librprof_preload.a \
		-Wl,--exclude-libs,ALL -ldl -lpthread -lm -lrt -lutil -o "$$temporary"; \
		chmod 755 "$$temporary"; mv "$$temporary" "$(PRELOAD)"

mpi:
	$(CARGO) build -p rprof-mpi $(CARGO_FLAGS)
	@set -eu; temporary=$$(mktemp "target/$(PROFILE)/librprof_mpi.so.XXXXXX"); \
		trap 'rm -f "$$temporary"' EXIT; \
		$(MPICC) -std=c11 -O2 -g -fPIC -shared -Wall -Wextra -Werror \
		crates/rprof-mpi/native/wrappers.c target/$(PROFILE)/librprof_mpi.a \
		-Wl,--exclude-libs,ALL -ldl -lpthread -lm -lrt -lutil -o "$$temporary"; \
		chmod 755 "$$temporary"; mv "$$temporary" "target/$(PROFILE)/librprof_mpi.so"

ompt:
	$(CARGO) build -p rprof-ompt $(CARGO_FLAGS)
	@set -eu; temporary=$$(mktemp "target/$(PROFILE)/librprof_ompt.so.XXXXXX"); \
		trap 'rm -f "$$temporary"' EXIT; \
		$(CC) -std=c11 -O2 -g -fPIC -shared -Wall -Wextra -Werror \
		crates/rprof-ompt/native/tool.c target/$(PROFILE)/librprof_ompt.a \
		-Wl,--exclude-libs,ALL -ldl -lpthread -lm -lrt -lutil -o "$$temporary"; \
		chmod 755 "$$temporary"; mv "$$temporary" "target/$(PROFILE)/librprof_ompt.so"

test: preload
	RPROF_TEST_IO_LIBRARY=$(CURDIR)/$(PRELOAD) $(CARGO) test --workspace
