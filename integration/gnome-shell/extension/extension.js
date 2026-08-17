import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

const SERVICE = 'io.github.agentsusagetray.App';
const PATH = '/io/github/agentsusagetray/App';
const IFACE = 'io.github.agentsusagetray.GnomeBridge1';
const DTP_TEMPORARY_HOLD = 1;

export default class AgentsUsageExtension extends Extension {
    enable() {
        this._heldPanels = [];
        this._visibilityPollId = 0;

        this._indicator = new PanelMenu.Button(0.5, this.metadata.name, false);
        this._indicator.visible = false;
        this._icon = new St.Icon({
            gicon: Gio.icon_new_for_string(`${this.path}/icons/bot-symbolic.svg`),
            style_class: 'system-status-icon',
            icon_size: 18,
        });
        this._indicator.add_child(this._icon);

        this._indicator.menu.addAction('Open', async () => {
            try {
                const visible = await this._callWithGeometry('OpenAt');
                this._syncPanelHold(visible);
            } catch (error) { this._reportError(error); }
        });
        this._indicator.menu.addAction('Refresh', () => {
            this._call('Refresh', null).catch(error => this._reportError(error));
        });
        this._indicator.menu.addAction('Settings', async () => {
            try {
                const visible = await this._callWithGeometry('OpenSettingsAt');
                this._syncPanelHold(visible);
            } catch (error) { this._reportError(error); }
        });
        this._indicator.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._indicator.menu.addAction('Quit', () => {
            this._releaseDashToPanel();
            this._call('Quit', null).catch(error => this._reportError(error));
        });

        Main.panel.addToStatusArea(this.uuid, this._indicator);

        // Preserve the product's intentional left-click popover / right-click menu split.
        this._indicator._clickGesture?.set_enabled(false);
        this._buttonPressId = this._indicator.connect('button-press-event', (_actor, event) => {
            const button = event.get_button();
            if (button === Clutter.BUTTON_PRIMARY) {
                if (this._indicator.menu.isOpen)
                    this._indicator.menu.close();
                this._callWithGeometry('ToggleAt')
                    .then(visible => this._syncPanelHold(visible))
                    .catch(error => this._reportError(error));
                return Clutter.EVENT_STOP;
            }
            if (button === Clutter.BUTTON_SECONDARY) {
                this._indicator.menu.toggle();
                return Clutter.EVENT_STOP;
            }
            return Clutter.EVENT_PROPAGATE;
        });

        this._nameWatchId = Gio.bus_watch_name(
            Gio.BusType.SESSION,
            SERVICE,
            Gio.BusNameWatcherFlags.NONE,
            () => { if (this._indicator) this._indicator.visible = true; },
            () => {
                this._releaseDashToPanel();
                if (this._indicator) {
                    this._indicator.menu.close();
                    this._indicator.visible = false;
                }
            },
        );
    }

    disable() {
        this._releaseDashToPanel();
        if (this._nameWatchId) {
            Gio.bus_unwatch_name(this._nameWatchId);
            this._nameWatchId = 0;
        }
        if (this._indicator && this._buttonPressId) {
            this._indicator.disconnect(this._buttonPressId);
            this._buttonPressId = 0;
        }
        this._indicator?.destroy();
        this._indicator = null;
        this._icon = null;
    }

    _geometry() {
        const [x, y] = this._indicator.get_transformed_position();
        const [width, height] = this._indicator.get_transformed_size();
        const cx = x + width / 2;
        const cy = y + height / 2;
        const monitors = Main.layoutManager.monitors ?? [];
        let monitor = monitors.find(m =>
            cx >= m.x && cx < m.x + m.width && cy >= m.y && cy < m.y + m.height);
        monitor ??= Main.layoutManager.primaryMonitor;
        if (!monitor)
            monitor = {x: 0, y: 0, width: global.stage.width, height: global.stage.height};

        const distances = {
            top: Math.abs(y - monitor.y),
            bottom: Math.abs((monitor.y + monitor.height) - (y + height)),
            left: Math.abs(x - monitor.x),
            right: Math.abs((monitor.x + monitor.width) - (x + width)),
        };
        const edge = Object.entries(distances).sort((a, b) => a[1] - b[1])[0][0];
        const values = [x, y, width, height, monitor.x, monitor.y, monitor.width, monitor.height]
            .map(value => Math.round(value));
        return {values, edge};
    }

    async _callWithGeometry(method) {
        const geometry = this._geometry();
        const params = new GLib.Variant('(iiiiiiiis)', [...geometry.values, geometry.edge]);
        return await this._callBool(method, params);
    }

    async _callBool(method, params) {
        const result = await Gio.DBus.session.call(
            SERVICE, PATH, IFACE, method, params, new GLib.VariantType('(b)'),
            Gio.DBusCallFlags.NONE, 1500, null,
        );
        const unpacked = result.deep_unpack();
        return Boolean(unpacked[0]);
    }

    async _call(method, params) {
        await Gio.DBus.session.call(
            SERVICE, PATH, IFACE, method, params, null,
            Gio.DBusCallFlags.NONE, 1500, null,
        );
    }

    _candidateDashToPanelPanels() {
        const panels = global.dashToPanel?.panels ?? [];
        if (!panels.length)
            return [];

        let monitorIndex = -1;
        try {
            monitorIndex = Main.layoutManager.findIndexForActor?.(this._indicator) ?? -1;
        } catch (_) {}

        if (monitorIndex >= 0) {
            const matching = panels.filter(panel => panel?.monitor?.index === monitorIndex);
            if (matching.length)
                return matching;
        }
        return panels;
    }

    _holdDashToPanel() {
        if (this._heldPanels.length)
            return;
        const panels = this._candidateDashToPanelPanels();
        for (const panel of panels) {
            try {
                if (panel?.intellihide?.revealAndHold) {
                    panel.intellihide.revealAndHold(DTP_TEMPORARY_HOLD, true);
                    this._heldPanels.push(panel);
                }
            } catch (error) {
                console.debug(`[Agents Usage] Dash-to-Panel hold unavailable: ${error}`);
            }
        }
        if (this._heldPanels.length)
            this._startVisibilityPoll();
    }

    _releaseDashToPanel() {
        for (const panel of this._heldPanels) {
            try { panel?.intellihide?.release?.(DTP_TEMPORARY_HOLD); }
            catch (_) {}
        }
        this._heldPanels = [];
        if (this._visibilityPollId) {
            GLib.Source.remove(this._visibilityPollId);
            this._visibilityPollId = 0;
        }
    }

    _syncPanelHold(visible) {
        if (visible)
            this._holdDashToPanel();
        else
            this._releaseDashToPanel();
    }

    _startVisibilityPoll() {
        if (this._visibilityPollId)
            return;
        this._visibilityPollId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 250, () => {
            this._callBool('IsVisible', null)
                .then(visible => {
                    if (!visible)
                        this._releaseDashToPanel();
                })
                .catch(() => this._releaseDashToPanel());
            return this._heldPanels.length ? GLib.SOURCE_CONTINUE : GLib.SOURCE_REMOVE;
        });
    }

    _reportError(error) {
        if (error instanceof Gio.DBusError)
            Gio.DBusError.strip_remote_error(error);
        console.error(`[Agents Usage] ${error}`);
    }
}
