# Arquitectura de Alfone

Última actualización: 2026-11-05

## 1. Vista general

Alfone separa el trabajo crítico (entrada, red, medios) de la interfaz. La UI
nunca captura eventos ni mantiene la sesión: si la ventana se cierra, la sesión
sigue. Esa separación también permite usar el mismo núcleo desde la CLI, la UI
Tauri o un futuro servicio del sistema.

```
┌────────────────────────────── PC A (servidor) ──────────────────────────────┐
│                                                                             │
│  teclado/ratón ──► alfone-input (Capture) ──► router de bordes (alfone-core) │
│                                                        │                    │
│                                     ┌──────────────────┴─────────┐          │
│                                     │ local: no se envía nada    │          │
│                                     │ remoto: Message::Input     │          │
│                                     └──────────────────┬─────────┘          │
│                                                        ▼                    │
│                                            alfone-net (QUIC, TLS 1.3)       │
└────────────────────────────────────────────────────────┬────────────────────┘
                                                         │
                                    control: stream bidireccional (postcard)
                                    audio/vídeo: datagramas QUIC
                                                         │
┌────────────────────────────── PC B (cliente) ──────────▼────────────────────┐
│                                                                             │
│  alfone-net ──► Message::Input ──► alfone-input (Inject) ──► escritorio      │
│                                                                             │
│  pantalla/audio ──► alfone-media (futuro) ──► datagramas QUIC               │
└─────────────────────────────────────────────────────────────────────────────┘
```

## 2. Crates

| Crate | Responsabilidad | Depende de |
|---|---|---|
| `alfone-protocol` | Tipos del protocolo, framing, keysyms, paquetes de media. Sin async ni SO. | — |
| `alfone-core` | Configuración TOML, validación del layout, detección de bordes y cruce proporcional. | protocol |
| `alfone-net` | Identidad autofirmada, TLS con pinning, servidor/cliente QUIC, handshake y `Link`. | protocol |
| `alfone-input` | Traits `Capture`/`Inject` y backends por plataforma: Linux/X11 (XRecord + XTEST) y Windows (hooks + `SendInput`). | protocol |
| `alfone-clipboard` | Sincronización de portapapeles (stub en M0). | protocol |
| `alfone-media` | Audio Opus y vídeo (stub en M0; los formatos ya están en `protocol`). | protocol |
| `alfone-discovery` | Descubrimiento en LAN por mDNS (`_alfone._udp.local.`): anuncio con TXT y `browse`. | — |
| `alfone-cli` | Binario `alfone`: `init`, `doctor`, `discover`, `pair`, `serve`, `connect`, plano HTTP local. | todos |
| `apps/alfone-gui` | UI Tauri 2 + React (fuera del workspace); habla con el daemon por HTTP local. | IPC/HTTP |

`alfone-protocol` no conoce Tokio ni ningún SO. Eso permite testear el wire
format en cualquier máquina y reutilizarlo desde un cliente móvil.

## 3. Sesión KVM

### 3.1 Servidor (`alfone serve`)

1. Carga configuración e identidad, y escucha QUIC en `0.0.0.0:24800`.
2. Genera un PIN de seis dígitos para la sesión y muestra también la huella y
   el código corto del certificado.
3. Acepta un cliente, hace el handshake y, si el PIN está activo, espera
   `PairRequest`/`PairResponse`.
4. Recibe `Displays` del cliente para conocer el tamaño real de su pantalla.
5. Construye un layout de dos pantallas (`local` + `remote`) y lo envía.
6. Captura la entrada local. Cuando el puntero toca el borde compartido, envía
   `EnterScreen` con la posición de entrada y activa el modo remoto:
   - bloquea teclado/ratón locales con `XGrabKeyboard`/`XGrabPointer`;
   - recentra el cursor local para poder seguir generando movimiento relativo.
7. En modo remoto reenvía movimiento relativo, botones, rueda y teclas.
8. Vuelve al escritorio local cuando:
   - el cliente detecta que el puntero sale por un borde y envía
     `LeaveScreen`, o
   - el usuario pulsa `Ctrl+Alt+Esc`.

### 3.2 Cliente (`alfone connect`)

1. Conecta, verifica la huella del servidor (pinning o TOFU) y responde al PIN.
2. Anuncia el tamaño de su pantalla con `Displays`.
3. Inyecta cada `Message::Input` en el escritorio local (XTEST en X11).
4. Cuando una posición relativa intenta salir de la pantalla, envía
   `LeaveScreen` para devolver el control al servidor.

No captura entrada local en M0: el cliente puede seguir usándose con su propio
teclado, aunque un servidor conectado también puede inyectar eventos.

### 3.3 Plano de control UI ↔ daemon

La UI nunca forma parte del proceso que captura entrada: eso garantiza que
cerrar la ventana no interrumpa la sesión. En su lugar, `alfone serve` levanta
un servidor HTTP **solo en localhost** (`127.0.0.1:24801`, configurable):

- `GET /health` → estado del daemon (idle/activo).
- `GET /status` → identidad, huella, código corto y estado.
- `GET /layout` / `PUT /layout` → leer, validar y persistir el layout.
- Autenticación por `Authorization: Bearer <token>`; el token se genera en el
  primer arranque y se guarda en `[security] http_token`.

La app Tauri expone comandos `health`, `get_layout`, `set_layout` e
`ipc_base_url` que hacen de puente entre React y ese HTTP local. Así el daemon
y la UI se pueden actualizar/recompilar por separado.

## 4. Hilos y tareas

- **Captura**: hilo propio del backend (XRecord bloquea esperando eventos). Un
  hilo puente traduce el canal `std::sync::mpsc` a `tokio::sync::mpsc`.
- **Red**: runtime Tokio multi-hilo. `Link` lanza dos tareas, lector y escritor,
  y expone un canal de mensajes. El envío nunca bloquea al bucle de entrada más
  allá de la cola.
- **Inyección**: se hace en el bucle de sesión, de forma síncrona. En X11 son
  peticiones XTEST pequeñas; si en el futuro bloquea, se moverá a un hilo.
- **UI**: proceso aparte. Hablará con el daemon por un socket local (pendiente).

## 5. Configuración

Ruta por defecto: `~/.config/alfone/Alfone/config.toml` (Linux),
`%APPDATA%\alfone\Alfone\config.toml` (Windows) o
`~/Library/Application Support/dev.alfone.Alfone/config.toml` (macOS).

La identidad (certificado y clave privada) vive en `identity/` junto al archivo
de configuración. La clave se crea con permisos `0600` en Unix.

```toml
device_name = "desktop"
role = "Server"

[server]
listen = "0.0.0.0:24800"
discovery = true

[client]
server = "192.168.1.20:24800"
fingerprint = "..."

[layout]
primary = "local"

[audio]
enabled = false
sample_rate = 48000
channels = 2
frame_ms = 20
```

## 6. Estado actual y límites

Implementado y probado:

- protocolo y framing con tests;
- configuración, layout y router de bordes con tests;
- QUIC + TLS + identidad + pinning + PIN, con test de integración loopback;
- backend Linux/X11 (XRecord + XTEST + keymap + grab);
- CLI funcional con `init`, `doctor`, `pair`, `serve` y `connect`;
- cliente/servidor intercambiando eventos reales en la misma máquina.

Pendiente:

- probar el backend Windows en una máquina real (escrito y *type-checked* con
  el target `x86_64-pc-windows-msvc`; macOS queda con `CGEventTap` +
  `CGEventPost`);
- Wayland (libei + portal);
- UI Tauri real y su IPC con el daemon (el esqueleto ya habla con el daemon
  por el plano HTTP local; falta el build en una máquina con GTK/WebKit);
- audio, vídeo y portapapeles;
- driver de pantalla virtual para el modo "monitor extra".

Listo en M1 (2026-11-13):

- descubrimiento mDNS (`alfone discover`, `serve` anuncia, `connect --name`);
- reconexión automática en el cliente con backoff y pinning TOFU de la huella
  para el resto de la ejecución;
- keepalive/idle timeout QUIC afinados para LAN (detección de caída ~8 s).

## 7. Decisiones y porqués

- **QUIC en vez de TCP**: multiplexación sin bloquear el ratón por culpa del
  vídeo, datagramas para medios, TLS 1.3 y migración de conexión.
- **Keysyms X11 como identidad lógica de tecla**: mismo criterio que Input Leap;
  el `hid` y el `native` viajan aparte para diagnóstico y para layouts raros.
- **Coordenadas absolutas al cruzar, relativas dentro del remoto**: el cruce
  necesita saber dónde aparece el cursor; el movimiento continuo necesita
  deltas para no chocar con los límites de la pantalla.
- **Un servidor y N clientes, sin malla**: simplifica el layout y la seguridad;
  la malla entre clientes no aporta al caso de uso inicial.
