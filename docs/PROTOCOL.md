# Protocolo de Alfone

Última actualización: 2026-11-05

Versión actual: **1000** (`PROTOCOL_MAJOR = 10`, `PROTOCOL_MINOR = 0`).

## 1. Transporte

- **QUIC** (RFC 9000) sobre UDP, puerto por defecto **24800**.
- **TLS 1.3** obligatorio. Certificado autofirmado por dispositivo; el cliente
  verifica la huella SHA-256 (pinning) o aplica TOFU.
- ALPN: `alfone/1`.
- Un **stream bidireccional** por conexión transporta los mensajes de control.
- Los **datagramas QUIC** transportan audio y fragmentos de vídeo. No comparten
  cola con el control: la pérdida de un paquete de audio no retrasa el ratón.
- Los streams unidireccionales quedan reservados para transferencias grandes
  (portapapeles y archivos), una por transferencia.

## 2. Framing de control

Cada mensaje es:

```
┌──────────────────┬───────────────────────────────┐
│ longitud (u32 BE)│ cuerpo postcard (Message)     │
└──────────────────┴───────────────────────────────┘
```

- `postcard` es un formato binario compacto, sin nombres de campo y con
  varints. Un `PointerRel` ocupa menos de 8 bytes.
- La longitud anunciada no puede superar **8 MiB** (`MAX_FRAME_BYTES`). Un
  valor mayor cierra la conexión: es un error o un peer hostil.
- El receptor lee primero 4 bytes, valida la longitud y luego lee exactamente
  ese número de bytes. Un cierre limpio antes de los 4 bytes se interpreta como
  fin de stream; un cierre a medias es un error de protocolo.

## 3. Compatibilidad

`remote / 100 == PROTOCOL_MAJOR` significa compatible. Un peer con la misma
mayor (1007) puede hablar con 1000: los campos nuevos deben ser opcionales o
añadirse al final con valores por defecto. Cambiar la mayor rompe.

## 4. Handshake

```
cliente                                         servidor
   │                                               │
   │──────────── conexión QUIC + TLS ─────────────►│
   │                                               │
   │◄────────────── Hello ─────────────────────────│  protocol, device,
   │                                               │  capabilities, mode,
   │                                               │  pairing_required, nonce
   │────────────── HelloBack ─────────────────────►│  + server_fingerprint
   │                                               │
   │   si pairing_required:                        │
   │────────────── PairRequest(code) ─────────────►│
   │◄───────────── PairResponse(accepted) ─────────│
   │                                               │
   │◄══════════ control bidireccional ════════════►│
```

- El servidor abre el stream y envía `Hello`.
- El cliente responde `HelloBack` con la huella que verificó del certificado.
  El servidor comprueba que coincide con la suya (detecta un proxy TLS o un
  error de pinning).
- El PIN se compara ignorando guiones, mayúsculas y espacios
  (`normalize_code`). Un PIN incorrecto produce `PairResponse { accepted:false }`
  y se cierra la conexión.
- El `nonce` de ambas partes está pensado para derivar un *short authentication
  string* que el usuario pueda comparar; hoy se usa la huella del certificado.

## 5. Mensajes

Todos los mensajes viven en el enum `Message` (`alfone-protocol`).

| Mensaje | Dirección | Contenido |
|---|---|---|
| `Hello` | S→C | versión, `DeviceInfo`, capacidades, modo, `pairing_required`, nonce |
| `HelloBack` | C→S | versión, `DeviceInfo`, capacidades, modo, nonce, huella del servidor |
| `PairRequest` | C→S | código y `DeviceInfo` |
| `PairResponse` | S→C | `accepted`, motivo |
| `Ping` / `Pong` | ambos | `id: u64`; se responde con el mismo id |
| `Disconnect` | ambos | motivo legible |
| `Error` | ambos | `code: u16`, `message` |
| `Displays` | ambos | lista de pantallas con geometría y escala |
| `Layout` | S→C | `LayoutSnapshot` del servidor |
| `EnterScreen` | S→C | x, y, `sequence`, modificadores |
| `LeaveScreen` | C→S | `sequence` |
| `Input` | S→C | `InputEvent` |
| `Clipboard` | ambos | oferta, petición, trozo, fin, cancelación |
| `Audio` | ambos | start/stop/mute/volume |
| `Video` | ambos | start/stop/keyframe |
| `Mode` | ambos | `Kvm`, `Mirror` o `Extend` |

### 5.1 `InputEvent`

- `PointerAbs { x, y }`: coordenadas absolutas **en el espacio de la pantalla
  destino**.
- `PointerRel { dx, dy }`: deltas; el receptor los suma y recorta a su pantalla.
- `Button { button, pressed }`: `PointerButton` (Left/Middle/Right/Back/Forward/
  Other).
- `Wheel { dx, dy }`: 120 unidades por muesca; positivo = arriba/derecha.
- `Key(KeyEvent)`: acción (`Press`/`Release`/`Repeat`), `keysym` X11, `hid`
  (USB HID usage), `native` (scancode del emisor) y modificadores.

Los modificadores son un `bitflags` de 16 bits: Shift, Control, Alt, Meta,
Super, CapsLock, NumLock, Scroll.

### 5.2 `EnterScreen` / `LeaveScreen`

`EnterScreen` coloca el cursor del cliente en `(x, y)` y le dice que el servidor
tiene el control. `sequence` ordena estos mensajes respecto a `Input`, de modo
que un `Input` anterior al cruce no se aplique en la pantalla equivocada.

`LeaveScreen` lo envía el cliente cuando el cursor inyectado intenta salir por
un borde. El servidor recupera el control y coloca su cursor en el borde
correspondiente, usando la proporción del eje compartido.

## 6. Datagramas de audio

Cabecera de 8 bytes, big-endian:

```
u32 sequence | u32 timestamp_us | payload Opus (≤ 1150 bytes)
```

- 48 kHz, estéreo, frames de 10–20 ms.
- Secuencia para detectar pérdidas y reordenar; timestamp para el jitter buffer.
- El payload máximo mantiene el datagrama por debajo del MTU típico (1200 B).
- El receptor descarta paquetes fuera de la ventana de jitter (por defecto
  40 ms) y oculta pérdidas con PLC de Opus.

## 7. Datagramas de vídeo

Cabecera de 9 bytes, big-endian:

```
u32 frame_id | u16 index | u16 count | u8 flags | payload (≤ 1150 bytes)
```

- `flags bit 0`: keyframe; `flags bit 1`: último fragmento.
- Un frame se divide en `count` fragmentos. El receptor (`FrameAssembler`)
  reensambla por `frame_id`; si llega un frame nuevo antes de completar el
  anterior, descarta el incompleto.
- Para H.264 se pide un keyframe (`VideoControl::RequestKeyframe`) cuando falta
  un fragmento de un intra frame o tras una racha de pérdidas.

## 8. Errores y cierre

- Códigos de `Error`: `1` = versión de protocolo no soportada. El resto se
  reserva para autenticación, capacidades y límites.
- `Disconnect` es un cierre ordenado; el receptor debe dejar de inyectar.
- Cualquier frame mayor de 8 MiB o con longitud incoherente cierra la conexión.
- El pinning de la huella se hace en la capa TLS, antes de leer ningún mensaje.

## 9. Plano de control local (HTTP)

`alfone serve` levanta además un servidor HTTP en `127.0.0.1:24801` (configurable
en `[server] http_listen`) para que la UI de escritorio inspeccione y configure
el daemon sin formar parte de él. **Nunca se expone en la LAN.**

Autenticación: cabecera `Authorization: Bearer <token>`. El token (64 hex) se
genera en el primer `serve` y se guarda en `[security] http_token`; `serve` lo
imprime al arrancar. Sin token, todos los endpoints responden `401`.

| Método | Ruta | Descripción |
|---|---|---|
| GET | `/health` | `{ "ok": true, "state": 0\|1 }` (0 = idle, 1 = sesión activa) |
| GET | `/status` | Identidad, huella, código corto, puerto HTTP y estado |
| GET | `/layout` | Layout actual (`screens` + `primary`) |
| PUT | `/layout` | Valida y persiste un layout nuevo; `422` si es inválido |

Ejemplo:

```sh
TOKEN=$(grep http_token ~/.config/alfone/Alfone/config.toml | cut -d'"' -f2)
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:24801/status
```

La UI de Tauri habla con este plano mediante los comandos `health`,
`get_layout`, `set_layout` e `ipc_base_url` (peticiones a localhost).

## 10. Evolución prevista

1. Negociación de capacidades por dirección (hoy `Capabilities` es un conjunto
   plano).
2. ~~Descubrimiento mDNS (`_alfone._udp.local`) con TXT: id, nombre, huella
   corta.~~ **Hecho** (crate `alfone-discovery`, `alfone discover`).
3. Streams de portapapeles y archivos con hashes y límites.
4. Confirmación de identidad con SAS derivado de los nonces.
5. *Bridge* opcional con el protocolo 1.6 de Input Leap (puerto 24800/TCP) para
   convivir con instalaciones existentes.
