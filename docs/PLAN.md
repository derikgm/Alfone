# Plan de Alfone

Última actualización: 2026-11-05

## 1. Visión

Un solo teclado/ratón para todas tus máquinas, con el portapapeles, el audio y
la pantalla viajando por la red local. Cuatro modos, que se pueden combinar:

| Modo | Qué hace | Referencia |
|---|---|---|
| **KVM** | El cursor cruza el borde y controla otra máquina | Input Leap |
| **Clipboard** | Texto, HTML, imágenes y archivos entre máquinas | Synergy/Barrier |
| **Audio** | Envía el audio del PC A a los altavoces del PC B (y viceversa) | AudioRelay/SonoBus |
| **Segunda pantalla** | El PC B se comporta como un monitor extra del PC A | Spacedesk/Duet |

No es un objetivo inicial la paridad 1:1 con Synergy ni el soporte de sus
protocolos. La compatibilidad con Input Leap (protocolo 1.6) se puede añadir más
adelante como *bridge*; el protocolo de Alfone será propio y versionado.

## 2. Decisiones de arquitectura

### 2.1 Rust + daemon separado + Tauri

- **`alfone` (CLI/daemon)**: todo el trabajo crítico. Sin UI, arranca como
  servicio y puede correr en una máquina sin escritorio. Esto también permite
  testear y empaquetar por separado.
- **Tauri 2**: panel de control (emparejamiento, layout de pantallas, audio,
  estado) y visor de la segunda pantalla. La UI habla con el daemon por IPC
  local (socket/named pipe) o por comandos Tauri.
- **No** se mete la captura de entrada dentro del proceso de UI: los hooks de
  teclado y los permisos de accesibilidad son frágiles y deben sobrevivir al
  cierre de la ventana.

### 2.2 Transporte: QUIC, no TCP

QUIC (crate `quinn`, TLS 1.3) da casi gratis:

- Multiplexación sin *head-of-line blocking* (el vídeo no bloquea el ratón).
- Streams bidireccionales/unidireccionales por canal.
- **Datagramas** para audio y para fragmentos de vídeo con baja latencia.
- Control de congestión moderno y migración de conexión (cambio de Wi-Fi a
  Ethernet sin reconectar).
- TLS 1.3 integrado, con certificados autofirmados y *pinning* por huella.

Alternativas descartadas para v1:
- **WebRTC**: mejor para NAT/WAN y ecosistema de medios, pero en Rust
  (`webrtc-rs`) es más pesado, el *signaling* y el SFU multi-peer añaden mucha
  complejidad. Se puede usar como segundo transporte para vídeo en v3.
- **TCP crudo**: simple, pero obliga a reimplementar multiplexado, control de
  congestión y FEC. Es lo que ya hace Input Leap y no queremos repetirlo.

Los medios irán por **datagramas QUIC** con cabecera propia, números de
secuencia y un jitter buffer. Para pantalla completa se valorará WebRTC o un
stream QUIC con FEC adaptativa.

### 2.3 Vídeo y audio

- **Audio**: `cpal` para capturar/reproducir, **Opus** a 48 kHz, frames de
  10–20 ms, ~64–128 kbps. Captura de audio del sistema:
  - Windows: WASAPI loopback (`windows` crate).
  - macOS: ScreenCaptureKit (macOS 13+) o CoreAudio tap (14.4+); alternativa
    temporal: BlackHole.
  - Linux: monitor de PipeWire/PulseAudio (`.monitor`), o portal.
- **Vídeo**: H.264 de baja latencia como formato principal.
  - v1: JPEG por fragmentos (fácil, funciona en cualquier webview) para validar
    el pipeline extremo a extremo.
  - v2: H.264 (OpenH264/`openh264` para software; VAAPI/NVENC/VideoToolbox para
    hardware) y decodificación nativa.
  - La UI Tauri puede decodificar H.264 con WebCodecs cuando esté disponible;
    si no, se usa una superficie nativa o se sirve JPEG.
- **Segunda pantalla**: el huesped necesita una salida virtual. Es la parte más
  dependiente del SO (ver §5).

### 2.4 Seguridad desde el día 1

- Certificado autofirmado por dispositivo; huella SHA-256 mostrada como código
  de 8 caracteres (`ABCD-EFGH`).
- *Pinning* TOFU: la primera conexión exige confirmación del usuario; después
  se compara la huella guardada.
- Permisos explícitos por capacidad (entrada, portapapeles, audio, vídeo,
  archivos) y por peer.
- Nunca aceptar conexiones en silencio; indicador visible cuando la entrada
  remota está activa.
- Ver [`SECURITY.md`](SECURITY.md).

## 3. Estructura del repositorio

Ya creada:

```
alfone/
├── Cargo.toml
├── crates/alfone-protocol   # wire format puro y testeable
├── crates/alfone-core       # config, layout, edge router
├── crates/alfone-net        # QUIC + TLS + discovery + sesión
├── crates/alfone-input      # traits + backends por SO
├── crates/alfone-clipboard  # agente de portapapeles
├── crates/alfone-media      # Opus + vídeo
├── crates/alfone-cli        # binario `alfone`
├── apps/alfone-gui          # Tauri + React
└── docs/
```

Dependencias entre crates: `protocol` no depende de nada del proyecto; `core`
depende de `protocol`; `net` de `protocol`; `input`/`clipboard`/`media` de
`protocol`; `cli` de todos. La UI no depende del código Rust del daemon: habla
por IPC (así se puede actualizar el daemon sin recompilar la UI).

## 4. Hoja de ruta

Los tiempos son estimaciones para **una persona a tiempo completo**; en ratos
libres, multiplicar por 2–3.

### M0 — Fundación (1–2 semanas) ✅ en curso

- [x] Workspace, documentación y decisiones.
- [x] Protocolo versionado + framing + tests.
- [x] Configuración y layout de pantallas con router de bordes + tests.
- [x] Transporte QUIC con TLS autofirmado y pinning + test loopback.
- [x] CLI `alfone` (`init`, `doctor`, `pair`, `serve`, `connect`).
- [x] Backend Linux/X11 real: XRecord, XTEST, keymap y grab.
- [x] Documentación de arquitectura, protocolo y seguridad.
- [x] Esqueleto Tauri 2 + React (estado, identidad, PIN y vista del layout),
  ya conectado al daemon por el plano HTTP local.
- [ ] Empaquetado inicial (cargo-dist / Tauri bundler).
- [x] IPC real UI ↔ daemon: plano HTTP local en `alfone serve`
  (`/health`, `/status`, `/layout` con token bearer) y comandos Tauri
  (`health`, `get_layout`, `set_layout`, `ipc_base_url`).
- [x] Cablear la UI React a esos comandos y hacer editable la vista del layout
  (salud del daemon cada 5 s, pantallas reales con tamaño y botón de guardado).

**Criterio de salida:** dos procesos en la misma máquina intercambian eventos y
el layout decide correctamente el cruce de bordes; `cargo test` verde.

**Estado actual:** ✅ M0 funcional en verde — protocolo, core, red, input e IPC
skeleton compilan y testean sin errores; QUIC loopback verificado (handshake +
PIN + input roundtrip). El plano HTTP está probado end-to-end con `curl`
(401 sin token, `/status`, `GET/PUT /layout` con validación y persistencia) y la
UI React ya consume `health`/`get_layout`/`set_layout` (validada con
`tsc --noEmit` y `vite build`). Pendiente: empaquetado inicial y build de Tauri
en máquina con GTK/WebKit. *(checklist actualizado al cerrar la fase de UI)*

### M1 — KVM utilizable en LAN (4–6 semanas)

- Backend Linux/X11 completo (captura XRecord, inyección XTEST, keymap).
- Backend Windows (hooks `WH_KEYBOARD_LL`/`WH_MOUSE_LL`, `SendInput`).
  [x] Escrito y verificado con `cargo check -p alfone-input --target
  x86_64-pc-windows-msvc --all-targets` (sin errores ni avisos); sin pruebas en
  una máquina Windows real todavía (captura `rdev::grab`, inyección `enigo`,
  modo remoto vía flag global que el hook lee para tragar eventos locales).
- Backend macOS (CGEventTap + CGEventPost; permisos de Accesibilidad).
  *(pospuesto: fuera del alcance inicial Linux X11 + Windows 11)*
- Layout gráfico en la UI (arrastrar pantallas como en Input Leap).
- [x] Descubrimiento mDNS y emparejamiento con código
  (`alfone discover`, `serve --discovery`, `connect --name`; test red `--ignored`).
- Atajos para cambiar de pantalla y bloquear la entrada remota.
- Tests de latencia y *soak test* de 24 h.

**Criterio de salida:** Windows ↔ Linux comparten teclado/ratón en una LAN,
con reconexión automática y sin pérdida de teclas. macOS se valida después.

### M2 — Portapapeles y archivos (2–3 semanas)

- Texto/HTML/imagen con hashes y transferencia por streams separados.
- Límite de tamaño, consentimiento y limpieza.
- Arrastrar y soltar archivos (v2).

### M3 — Audio (3–5 semanas)

- Captura loopback por SO + Opus + jitter buffer + reproducción.
- Sincronización, volumen, selección de dispositivos.
- Audio bidireccional con cancelación de eco (AEC) opcional.
- Métricas: latencia < 40 ms en LAN, sin glitches en 1 h.

### M4 — Segunda pantalla (8–16 semanas, la más arriesgada)

- Windows: driver de display virtual (IddCx) + firma EV.
- Linux: `evdi`/DRM o `xrandr` con salida dummy; Wayland vía portal.
- macOS: `CGVirtualDisplay` (API privada, frágil) o pantalla virtual de terceros.
- Captura de la salida virtual + H.264 + visor Tauri.
- Modo "monitor remoto": el host ve la pantalla extra como nativa; el cliente
  solo muestra vídeo y reenvía input en coordenadas absolutas.

**Criterio de salida:** una sesión de trabajo de 8 h con el PC B como tercer
monitor, latencia de entrada < 15 ms y vídeo estable a 1080p30.

### M5 — Producto (4–6 semanas)

- Instaladores firmados (MSI/MSIX, DMG notarizado, .deb/AppImage/Flatpak).
- Autoarranque, bandeja del sistema, actualizaciones.
- Telemetría opt-in, logs rotativos, diagnóstico.
- Documentación de usuario y matriz de compatibilidad.

### Futuro

- Bridge con el protocolo de Input Leap.
- WAN con `iroh`/Tailscale (hole punching, relay).
- Transferencia de archivos grande, impresión remota, portapapeles ilimitado.
- Cifrado de extremo a extremo por peer y auditoría.

## 5. Segunda pantalla: realidad por sistema operativo

No basta con capturar la pantalla: el host debe **creer** que existe un monitor
extra. Eso requiere un driver o una API del sistema:

| SO | Mecanismo | Dificultad | Notas |
|---|---|---|---|
| Windows 10/11 | IddCx (Indirect Display Driver) | Alta | Driver en C/C++/Rust, firma EV, instalación con admin |
| Linux X11 | `xrandr --setmonitor` + salida dummy / `evdi` | Media | `evdi` es out-of-tree; Xorg acepta salidas virtuales |
| Linux Wayland | `xdg-desktop-portal` + `libei`/`wlr-virtual-pointer` | Media-Alta | Protocolos por compositor (GNOME/KDE/Sway) |
| macOS | `CGVirtualDisplay` (privada) | Alta | Sin API pública; riesgo de romperse y de App Store |

Estrategia: v1 **modo espejo** (streaming de una pantalla existente) para validar
el pipeline; v2 salida virtual por SO. El emparejamiento y el transporte son los
mismos en ambos modos.

## 6. Riesgos y mitigaciones

| Riesgo | Impacto | Mitigación |
|---|---|---|
| Wayland bloquea hooks globales | Alto en Linux moderno | `libei` + portal; detectar sesión y avisar; soportar X11 primero |
| Permisos macOS (Accesibilidad, Grabación de pantalla) | Alto | Onboarding guiado, `doctor`, detección y deep-links a Ajustes |
| Drivers de pantalla virtual | Muy alto | Fase separada, firmas, no bloquear el resto del producto |
| Latencia de entrada | Alto | Hilos dedicados, prioridad, sin UI en el camino, presupuesto < 10 ms LAN |
| Calidad/latencia de vídeo | Alto | H.264 low-latency, hardware cuando exista, medir con métricas |
| Audio del sistema por SO | Medio | Backends nativos; no depender solo de `cpal` |
| Seguridad (input remoto) | Crítico | TOFU + confirmación, permisos por capacidad, botón de pánico |
| Complejidad multi-peer | Medio | v1 un servidor + N clientes; sin malla entre clientes |

## 7. Presupuesto de latencia (objetivo LAN)

```
captura del evento        1–3 ms
cola + serialización      0.2 ms
red (Gigabit/Ethernet)    0.3–1 ms
inyección en el cliente   1–2 ms
--------------------------------
total entrada             3–7 ms  (objetivo < 10 ms)

audio: captura 10 ms + Opus 2–5 ms + red 1 ms + jitter 20 ms + salida 10 ms ≈ 40–50 ms
vídeo: captura 5 ms + encode 5–15 ms + red 2 ms + decode 5–15 ms + present 8–16 ms ≈ 25–55 ms
```

## 8. Qué se ha implementado en esta sesión

- Workspace y documentación (`README`, `PLAN`, `ARCHITECTURE`, `PROTOCOL`,
  `SECURITY`).
- `alfone-protocol`: mensajes, versionado, framing con límite, claves/keysyms,
  paquetes de audio y fragmentos de vídeo, tests.
- `alfone-core`: configuración TOML, layout con validación y router de bordes
  (cruce proporcional), snapshot serializable, tests.
- `alfone-net`: endpoint QUIC, identidad autofirmada persistente, huella y
  código de emparejamiento, cliente/servidor con pinning, test de integración.
- `alfone-cli`: comandos `serve`, `connect`, `pair`, `doctor`, `init`; el
  servidor enruta la entrada por bordes, el cliente la inyecta y ambos vuelven
  con `Ctrl+Alt+Esc` o al salir por el borde.
- `alfone-input`: traits, backend mock y backend Linux/X11 completo.
- `apps/alfone-gui`: esqueleto Tauri 2 + React con estado, identidad, generador
  de PIN y vista previa del layout.
- `alfone-media`, `alfone-clipboard`: stubs; los formatos de audio/vídeo ya
  están definidos en `alfone-protocol`.

Lo que **no** está: backends Windows/macOS/Wayland, IPC UI ↔ daemon, audio real,
vídeo real y driver de pantalla virtual. Son los siguientes hitos.

### Probarlo ya (Linux/X11, dos terminales)

```bash
# Terminal 1: servidor, genera un PIN
cargo run -p alfone-cli -- serve

# Terminal 2: cliente en la misma máquina
cargo run -p alfone-cli -- connect --server 127.0.0.1:24800 --pin <PIN>
```

Mueve el ratón hasta el borde derecho: el control pasa al cliente. En una sola
máquina el efecto es sutil; la prueba real es entre dos equipos. `Ctrl+Alt+Esc`
recupera el control local.

## 9. Decisiones tomadas

1. **Prioridad:** KVM en LAN robusto primero; audio/vídeo y portapapeles después.
2. **Segunda pantalla:** modo **espejo** en v1 (sin driver); el monitor virtual
   queda para M4.
3. **Red:** solo LAN en v1. WAN (iroh/Tailscale, relay) queda para el futuro.
4. **Nombre:** se mantiene **Alfone**.
5. **Plataformas iniciales:** **Linux X11 + Windows 11**. macOS y Wayland
   quedan para después de M1.

Estas respuestas actualizan el alcance de M1 (Windows ↔ Linux, sin macOS) y
confirman el modo espejo de M4.
