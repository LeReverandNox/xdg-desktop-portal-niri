BIN_NAME := xdg-desktop-portal-niri

BINDIR ?= $(HOME)/.local/bin
DBUS_SERVICES_DIR ?= $(HOME)/.local/share/dbus-1/services
SYSTEMD_USER_DIR ?= $(HOME)/.config/systemd/user
PORTALS_DIR ?= $(HOME)/.local/share/xdg-desktop-portal/portals
PORTAL_CONF ?= $(HOME)/.config/xdg-desktop-portal/niri-portals.conf

# Rust toolchain lives in the devbox environment (rustup), not on the host.
DEVBOX := devbox run --

.PHONY: all build toolchain install uninstall clean

all: build

# Idempotent: only downloads/switches on the first run.
toolchain:
	$(DEVBOX) rustup default stable

build: toolchain
	$(DEVBOX) cargo build --release

install: build
	install -Dm755 target/release/$(BIN_NAME) $(BINDIR)/$(BIN_NAME)

	mkdir -p $(DBUS_SERVICES_DIR)
	sed 's|@BINDIR@|$(BINDIR)|' packaging/org.freedesktop.impl.portal.desktop.niri.service.in \
		> $(DBUS_SERVICES_DIR)/org.freedesktop.impl.portal.desktop.niri.service

	mkdir -p $(SYSTEMD_USER_DIR)
	sed 's|@BINDIR@|$(BINDIR)|' packaging/xdg-desktop-portal-niri.service.in \
		> $(SYSTEMD_USER_DIR)/xdg-desktop-portal-niri.service

	install -Dm644 packaging/niri.portal $(PORTALS_DIR)/niri.portal

	@mkdir -p $(dir $(PORTAL_CONF))
	@if [ ! -f $(PORTAL_CONF) ]; then \
		printf '[preferred]\norg.freedesktop.impl.portal.ScreenCast=niri;\n' > $(PORTAL_CONF); \
		echo "Created $(PORTAL_CONF)"; \
	elif ! grep -qxF 'org.freedesktop.impl.portal.ScreenCast=niri;' $(PORTAL_CONF); then \
		printf 'org.freedesktop.impl.portal.ScreenCast=niri;\n' >> $(PORTAL_CONF); \
		echo "Added ScreenCast override to $(PORTAL_CONF)"; \
	fi

	systemctl --user daemon-reload
	systemctl --user restart xdg-desktop-portal-niri
	systemctl --user restart xdg-desktop-portal

uninstall:
	-systemctl --user stop xdg-desktop-portal-niri
	rm -f $(BINDIR)/$(BIN_NAME)
	rm -f $(DBUS_SERVICES_DIR)/org.freedesktop.impl.portal.desktop.niri.service
	rm -f $(SYSTEMD_USER_DIR)/xdg-desktop-portal-niri.service
	rm -f $(PORTALS_DIR)/niri.portal
	systemctl --user daemon-reload
	@echo "Note: $(PORTAL_CONF) was left untouched -- remove the ScreenCast=niri; line by hand if desired."

clean:
	$(DEVBOX) cargo clean
