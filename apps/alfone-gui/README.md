# Alfone GUI

Panel de control de escritorio (Tauri 2 + React + TypeScript). Muestra el estado
del equipo, la identidad, un generador de PIN y una vista previa del layout.

> **Estado:** esqueleto de M0. Todavía no arranca ni controla la sesión: eso lo
> hace el daemon `alfone`. La comunicación UI ↔ daemon por IPC local es la
> siguiente tarea de este componente.

## Requisitos

- Rust estable y Node.js 20+ / pnpm.
- Linux: `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf`
  (Debian/Ubuntu/Mint).
- Windows: WebView2 (incluido en Windows 10/11) y MSVC build tools.
- macOS: Xcode Command Line Tools.

## Desarrollo

```bash
pnpm install
pnpm tauri dev
```

La app se compila aparte del workspace principal para que `cargo test
--workspace` funcione en máquinas sin GTK/WebKit.

## Comandos Rust expuestos

| Comando | Devuelve |
|---|---|
| `status` | Configuración, identidad y plataforma (sin crear nada) |
| `init_device` | Crea `config.toml` e identidad, y devuelve el estado |
| `new_pin` | PIN de seis dígitos para el flujo de emparejamiento |

## Siguiente paso

Añadir un socket local (Unix domain socket / named pipe) en el daemon y
comandos Tauri que lo consulten: estado de la sesión, iniciar/detener servidor,
layout activo y métricas de latencia. La UI no debe capturar entrada nunca.
