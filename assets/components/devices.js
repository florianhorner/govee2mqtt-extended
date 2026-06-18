import { LitElement, html} from "lit";
import { Task } from '@lit/task';
import { timeAgo } from './timeago.js';

const LAN_HELP_URL = 'https://github.com/florianhorner/govee2mqtt-extended/blob/main/docs/LAN.md';

export class DeviceList extends LitElement {
  timer;
  deviceList;

  static properties = {
    id: { type: String },
    label: { type: String },
    value: { type: String },
  };

  constructor() {
    super();
    this.value = "";
  }

  _deviceListTask = new Task(this, {
    task: async ([], {signal}) => {
      const response = await fetch('/api/devices', {signal});
      if (!response.ok) {
        throw new Error(response.status);
      }
      return response.json();
    },
    args: () => []
  });

  render() {
    return this._deviceListTask.render({
      pending: () => {
        if (this.deviceList === undefined) {
          return html`<p>Loading devices...</p>`;
        }
        return this._render_device_list(this.deviceList);
      },
      complete: (devices) => {
        this.deviceList = devices;
        return this._render_device_list(this.deviceList);
      }
    });
  }

  // This causes the element to appear in the normal DOM which gives it
  // access to the imported bootstrap CSS
  // https://stackoverflow.com/a/58462176/149111
  createRenderRoot() {
    return this;
  }

  ensureTimerStarted() {
    if (this.timer === undefined) {
      this.timer = setInterval(() => {
        this._deviceListTask.run();
      }, 5000);
    }
  }

  ensureTimerStopped() {
    clearInterval(this.timer);
    this.timer = undefined;
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    this.ensureTimerStopped();
  }

  connectedCallback() {
    super.connectedCallback();
    this.ensureTimerStarted();
  }

  _set_power_on(e) {
    const device_id = e.target.dataset.id;
    const power = e.target.checked ? 'on' : 'off';
    fetch(`/api/device/${device_id}/power/${power}`);
  }

  _set_color(e) {
    const device_id = e.target.dataset.id;
    const color = encodeURIComponent(e.target.value);
    console.log(`color will change to ${color}`);
    fetch(`/api/device/${device_id}/color/${color}`);
  }

  _render_badge(label, active, activeClass = 'text-bg-primary') {
    const badgeClass = active ? activeClass : 'text-bg-secondary';
    return html`<span class="badge rounded-pill ${badgeClass}">${label}</span>`;
  }

  _render_status_summary(devices) {
    const mqttConnected = devices.some((item) => item.mqtt_connected);
    const apiCount = devices.filter((item) => item.api_available).length;
    const lanCount = devices.filter((item) => item.lan_active).length;

    return html`
      <div class="device-status-summary mb-3">
        ${this._render_badge('MQTT', mqttConnected)}
        ${this._render_badge(`API ${apiCount}/${devices.length}`, apiCount > 0, 'text-bg-info')}
        ${this._render_badge(`LAN ${lanCount}/${devices.length}`, lanCount > 0, 'text-bg-info')}
      </div>`;
  }

  _room_groups(devices) {
    const groups = new Map();
    for (const item of devices) {
      const room = item.room || 'Unassigned';
      if (!groups.has(room)) {
        groups.set(room, []);
      }
      groups.get(room).push(item);
    }
    return [...groups.entries()];
  }

  _render_item = (item) => {
    const color_value = (item.state?.color.r << 16) | (item.state?.color.g << 8) | (item.state?.color.b);
    const rgb_hex = `#${color_value.toString(16).padStart(6, '0')}`;
    const rgb = item.state ? `rgba(${item.state.color.r}, ${item.state.color.g}, ${item.state.color.b}, ${item.state.brightness})`: null;
    const styles =  {
      backgroundColor: rgb,
    };

    const updated = item.state ?
      html`${timeAgo(new Date(item.state.updated))}` : html``;

    const source = item.state ?
      html`<span class="badge rounded-pill text-bg-info">${item.state.source}</span>` :
      html`<a href=${LAN_HELP_URL} target="_blank" rel="noopener" class="badge rounded-pill text-bg-warning text-decoration-none">Missing</a>`;

    const cloud = item.cloud_online === undefined || item.cloud_online === null ?
      html`` :
      this._render_badge(item.cloud_online ? 'Cloud online' : 'Cloud offline', item.cloud_online);

    const power_switch = html`
    <span class="form-switch"><input
      data-id=${item.id}
      class="form-check-input"
      type="checkbox"
      role="switch"
      @click=${this._set_power_on}
      ?checked=${item.state?.on}
    ></span>`;

    const color_picker = html`
      <input
        class="form-control form-control-color"
        data-id=${item.id}
        @change=${this._set_color}
        type="color"
        value=${rgb_hex}>
      `;

    return html`
        <tr class=${item.state ? '' : 'table-warning'}>
          <td>${item.name}</td>
          <td>${item.ip}</td>
          <td>${item.sku}</td>
          <td>${power_switch}</td>
          <td>${color_picker}</td>
          <td><tt>${item.id}</tt></td>
          <td style="width: 10em">${updated}</td>
          <td>
            <span class="device-row-badges">
              ${source}
              ${this._render_badge('API', item.api_available, 'text-bg-info')}
              ${this._render_badge('LAN', item.lan_active, 'text-bg-info')}
              ${cloud}
            </span>
          </td>
        </tr>
        `;
  }

  _render_device_list = (devices) => {
    return html`
        ${this._render_status_summary(devices)}
        ${this._room_groups(devices).map(([room, items]) => html`
          <section class="device-room-section mb-4">
            <h2>${room}</h2>
            <div class="table-responsive">
              <table class='table table-sm align-middle'>
                <thead>
                  <tr>
                    <th scope="col">Name</th>
                    <th scope="col">IP</th>
                    <th scope="col">SKU</th>
                    <th scope="col">Power</th>
                    <th scope="col">Color</th>
                    <th scope="col">ID</th>
                    <th scope="col">Last Updated</th>
                    <th scope="col">Status</th>
                  </tr>
                </thead>
                <tbody>
                  ${items.map(this._render_item)}
                </tbody>
              </table>
            </div>
          </section>
        `)}
          `;
  }
}

customElements.define("gv-device-list", DeviceList);
