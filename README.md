# Alfone

> KVM por software con esteroides: teclado/ratón, portapapeles, audio y una
> segunda pantalla, entre Windows, macOS y Linux.

Alfone nace como un clon moderno de [Input Leap](../input-leap): la misma idea
(controlar varias máquinas con un solo teclado y ratón) pero con un núcleo en
Rust, transporte QUIC, audio, vídeo y una UI Tauri.

**Estado:** M0 (protocolo, transporte QUIC, CLI y backend Linux/X11). Ya se
puede compartir teclado/ratón en una LAN entre máquinas Linux/X11; la UI, el
audio, el vídeo y los backends Windows/macOS están en la hoja de ruta
([`docs/PLAN.md`](docs/PLAN.md)).

## Por qué una arquitectura distinta a Input Leap

Input Leap es un excelente punto de partida, pero su protocolo TCP con
multiplexado manual y su modelo de hilos hacen caro añadir audio y vídeo de baja
latencia. Alfone separa:

- **Daemon nativo** (`alfone-cli`, sin UI): captura, inyección, red, medios.
- **UI Tauri** (`apps/alfone-gui`): panel de control, emparejamiento y visor.
- **QUIC** (quinn): TLS 1.3, multiplexación real, datagramas para audio/vídeo,
  control de congestión y migración de conexión.
- **Capas de plataforma** detrás de traits: Linux/X11, Wayland (libei + portal),
  Windows, macOS.

Leer el diseño completo en [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Estructura

```
alfone/
├── crates/
│   ├── alfone-protocol/   # mensajes, framing, claves, paquetes de media
│   ├── alfone-core/       # configuración, layout de pantallas, router de bordes
│   ├── alfone-net/        # QUIC, TLS con pinning, descubrimiento, sesiones
│   ├── alfone-input/      # captura e inyección por plataforma
│   ├── alfone-clipboard/  # sincronización de portapapeles
│   ├── alfone-media/      # audio Opus y vídeo (H.264/JPEG)
│   └── alfone-cli/        # daemon headless (`alfone`)
├── apps/alfone-gui/       # Tauri 2 + React/TypeScript
└── docs/                  # plan, arquitectura, protocolo, seguridad
```

## Compilar y probar el núcleo

```bash
cargo test --workspace
cargo run -p alfone-cli -- --help
```

En Linux, la captura/inyección X11 necesita un servidor X (Xorg). Wayland se
abordará con `libei`/portal en una fase posterior.

## Uso rápido

```bash
# Una vez por máquina: configuración e identidad
cargo run -p alfone-cli -- init
cargo run -p alfone-cli -- doctor

# PC A (el que tiene el teclado/ratón): muestra un PIN
cargo run -p alfone-cli -- serve

# PC B: conecta con el PIN del PC A
cargo run -p alfone-cli -- connect --server 192.168.1.20:24800 --pin 123456
```

Mueve el puntero hasta el borde derecho del PC A para controlar el PC B.
`Ctrl+Alt+Esc` devuelve el control al PC A, `Ctrl+Alt+flechas` cambia de
pantalla y `Ctrl+Alt+L` bloquea/desbloquea la entrada remota. `alfone connect`
recuerda la huella del servidor si antes ejecutaste `alfone pair`.

Para medir la calidad del enlace sin iniciar una sesión KVM:

```bash
cargo run -p alfone-cli -- latency --server 192.168.1.20:24800 --pin 123456 \
  --count 50 --interval 100 --timeout 2000
```

Imprime el RTT de cada ping y un resumen con min, avg, p50, p95, max y pérdida.

Para una prueba prolongada, ejecuta `./scripts/soak.sh server` en el PC A y
`./scripts/soak.sh client --server <ip> --pin <pin>` en el PC B. Por defecto
dura 24 h; usa `--seconds 600` para una prueba corta. El script guarda logs y
un resumen con reconexiones y errores en `logs/soak/`.

La entrada remota se inyecta con XTEST en Linux y `SendInput` en Windows, así
que el PC B también puede usarse con su propio teclado. La reconexión
automática ya está incluida; macOS y Wayland se abordarán en hitos posteriores.

## Compilar la UI (requiere dependencias del sistema)

```bash
cd apps/alfone-gui
pnpm install
pnpm tauri dev
```

Linux: `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf`.
Windows: WebView2 (viene con Windows 10/11). macOS: Xcode CLT.

Notas del entorno de desarrollo:

- pnpm 12 ignora el campo `pnpm` de `package.json`; las dependencias con
  scripts de build se aprueban en `pnpm-workspace.yaml` con `allowBuilds`
  (por eso `esbuild: true` está ahí).
- Si el repo vive en una partición NTFS vía FUSE (`fuseblk`), compilar ahí es
  muy lento y puede bloquear la I/O. Redirige el `target` a disco nativo:
  `CARGO_TARGET_DIR="$HOME/.cache/alfone-target" pnpm tauri dev`.
- Si `node` viene de snap y falla con `snap-confine ... cap_dac_override`,
  instala un Node oficial en `~/.local/opt` y ponlo primero en el `PATH`.

## Licencia

MIT OR Apache-2.0. Input Leap es GPL-2.0; este proyecto es una implementación
independiente y no reutiliza su código.
