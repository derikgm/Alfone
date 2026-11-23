import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Side = "left" | "right" | "top" | "bottom";

type Status = {
  initialized: boolean;
  configPath: string;
  deviceName: string;
  deviceId: string;
  platform: string;
  fingerprint: string | null;
  shortCode: string | null;
};

type Health = {
  ok: boolean;
  state: number;
};

type LayoutScreen = {
  id: string;
  name: string;
  width: number;
  height: number;
  left: string | null;
  right: string | null;
  top: string | null;
  bottom: string | null;
};

type Layout = {
  screens: LayoutScreen[];
  primary: string;
};

const demoStatus: Status = {
  initialized: false,
  configPath: "~/.config/alfone/Alfone/config.toml",
  deviceName: "demo",
  deviceId: "—",
  platform: "LinuxX11",
  fingerprint: null,
  shortCode: null,
};

const demoLayout: Layout = {
  screens: [
    {
      id: "local",
      name: "Este equipo",
      width: 1920,
      height: 1080,
      left: null,
      right: null,
      top: null,
      bottom: null,
    },
    {
      id: "remote",
      name: "Remoto",
      width: 1280,
      height: 800,
      left: null,
      right: null,
      top: null,
      bottom: null,
    },
  ],
  primary: "local",
};

function isTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

export default function App() {
  const [status, setStatus] = useState<Status | null>(null);
  const [health, setHealth] = useState<Health | null>(null);
  const [layout, setLayout] = useState<Layout | null>(null);
  const [pin, setPin] = useState<string>("");
  const [side, setSide] = useState<Side>("right");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      if (!isTauri()) {
        setStatus(demoStatus);
        setHealth({ ok: true, state: 0 });
        setLayout(demoLayout);
        return;
      }
      const next = await invoke<Status>("status");
      setStatus(next);
      try {
        setLayout(await invoke<Layout>("get_layout"));
      } catch {
        /* the daemon is not running yet: keep the current layout */
      }
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Poll the local daemon so the header reflects whether `alfone serve` is up.
  // When it appears, pull the layout once: the UI may have started first.
  const wasOnline = useRef(false);
  useEffect(() => {
    if (!isTauri()) {
      return;
    }
    let cancelled = false;
    const poll = async () => {
      try {
        const next = await invoke<Health>("health");
        if (cancelled) {
          return;
        }
        setHealth(next);
        if (!wasOnline.current) {
          wasOnline.current = true;
          void refresh();
        }
      } catch {
        if (!cancelled) {
          wasOnline.current = false;
          setHealth(null);
        }
      }
    };
    void poll();
    const id = window.setInterval(() => void poll(), 5000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [refresh]);

  const initialize = async () => {
    setBusy(true);
    try {
      if (!isTauri()) {
        setStatus({ ...demoStatus, initialized: true });
      } else {
        const next = await invoke<Status>("init_device");
        setStatus(next);
      }
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const generatePin = async () => {
    try {
      if (!isTauri()) {
        setPin(String(Math.floor(100000 + Math.random() * 900000)));
      } else {
        setPin(await invoke<string>("new_pin"));
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const copy = async (value: string, label: string) => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(label);
      window.setTimeout(() => setCopied(null), 1400);
    } catch {
      /* clipboard may be unavailable in the preview */
    }
  };

  const serverCommand =
    pin.length > 0 ? `alfone serve --pin ${pin}` : "alfone serve";

  const localScreen =
    layout?.screens.find((screen) => screen.id === layout.primary) ??
    layout?.screens[0];
  const remoteScreen = layout?.screens.find(
    (screen) => screen.id !== localScreen?.id,
  );

  const sideLabel: Record<Side, string> = {
    left: "izquierda",
    right: "derecha",
    top: "arriba",
    bottom: "abajo",
  };

  const saveLayout = async () => {
    if (!layout || !localScreen || !remoteScreen) {
      return;
    }
    const opposite: Record<Side, Side> = {
      left: "right",
      right: "left",
      top: "bottom",
      bottom: "top",
    };
    const link = (screen: LayoutScreen, direction: Side, neighbor: string) => {
      const next: LayoutScreen = {
        ...screen,
        left: null,
        right: null,
        top: null,
        bottom: null,
      };
      next[direction] = neighbor;
      return next;
    };
    const screens = layout.screens.map((screen) => {
      if (screen.id === localScreen.id) {
        return link(screen, side, remoteScreen.id);
      }
      if (screen.id === remoteScreen.id) {
        return link(screen, opposite[side], localScreen.id);
      }
      return { ...screen, left: null, right: null, top: null, bottom: null };
    });

    setBusy(true);
    try {
      if (!isTauri()) {
        setLayout({ ...layout, screens });
      } else {
        const updated = await invoke<Layout>("set_layout", {
          screens,
          primary: layout.primary,
        });
        setLayout(updated);
      }
      setSaved(true);
      setError(null);
      window.setTimeout(() => setSaved(false), 1800);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="app">
      <header className="hero">
        <div className="brand">
          <div className="logo">A</div>
          <div>
            <h1>Alfone</h1>
            <p className="tagline">
              Un teclado. Todas tus máquinas. Audio y pantallas incluidas.
            </p>
          </div>
        </div>
        <div className="state">
          <span className={`dot ${status?.initialized ? "ok" : "idle"}`} />
          {status?.initialized ? "Equipo preparado" : "Sin inicializar"}
          {isTauri() && (
            <span className={health ? "daemon online" : "daemon offline"}>
              ·{" "}
              {health
                ? health.state === 1
                  ? "sesión activa"
                  : "daemon en línea"
                : "daemon apagado"}
            </span>
          )}
        </div>
      </header>

      {error && (
        <div className="banner error">
          <strong>Error:</strong> {error}
        </div>
      )}

      {!isTauri() && (
        <div className="banner note">
          Vista previa en el navegador. Las acciones reales requieren{" "}
          <code>pnpm tauri dev</code>.
        </div>
      )}

      <section className="grid">
        <article className="card identity">
          <header>
            <h2>Este equipo</h2>
            <button className="ghost" onClick={() => void refresh()}>
              Actualizar
            </button>
          </header>

          <dl>
            <div>
              <dt>Nombre</dt>
              <dd>{status?.deviceName ?? "…"}</dd>
            </div>
            <div>
              <dt>Plataforma</dt>
              <dd>{status?.platform ?? "…"}</dd>
            </div>
            <div>
              <dt>ID</dt>
              <dd className="mono">{status?.deviceId ?? "—"}</dd>
            </div>
            <div>
              <dt>Configuración</dt>
              <dd className="mono small">{status?.configPath ?? "…"}</dd>
            </div>
          </dl>

          {status?.fingerprint ? (
            <>
              <div className="code">{status.shortCode}</div>
              <div className="fingerprint mono">{status.fingerprint}</div>
              <button
                className="ghost"
                onClick={() => void copy(status.fingerprint!, "fingerprint")}
              >
                {copied === "fingerprint" ? "Copiada" : "Copiar huella"}
              </button>
            </>
          ) : (
            <button
              className="primary"
              disabled={busy}
              onClick={() => void initialize()}
            >
              {busy ? "Preparando…" : "Inicializar equipo"}
            </button>
          )}
        </article>

        <article className="card pairing">
          <header>
            <h2>Emparejar</h2>
            <span className="badge">PIN por sesión</span>
          </header>
          <p className="muted">
            Arranca el servidor en el equipo que tiene el teclado. El PIN cambia
            en cada sesión y se muestra en su terminal.
          </p>
          <div className="pin-row">
            <div className="pin">{pin || "••••••"}</div>
            <button className="primary" onClick={() => void generatePin()}>
              Generar PIN
            </button>
          </div>
          <div className="command">
            <code>{serverCommand}</code>
            <button
              className="ghost small"
              onClick={() => void copy(serverCommand, "command")}
            >
              {copied === "command" ? "Copiado" : "Copiar"}
            </button>
          </div>
          <p className="hint">
            En el otro equipo:{" "}
            <code>alfone connect --server IP:24800 --pin {pin || "PIN"}</code>
          </p>
        </article>

        <article className="card layout">
          <header>
            <h2>Pantallas</h2>
            <span className="badge">
              {layout ? `${layout.screens.length} pantalla(s)` : "KVM"}
            </span>
          </header>
          <div className={`canvas side-${side}`}>
            <div className="screen local">
              <span>{localScreen?.name ?? "Este equipo"}</span>
              <small>
                {localScreen
                  ? `${localScreen.width}×${localScreen.height}`
                  : "…"}
              </small>
            </div>
            <div className="screen remote">
              <span>{remoteScreen?.name ?? "Remoto"}</span>
              <small>
                {remoteScreen
                  ? `${remoteScreen.width}×${remoteScreen.height}`
                  : "sin configurar"}
              </small>
            </div>
          </div>
          <div className="sides">
            {(["left", "right", "top", "bottom"] as Side[]).map((value) => (
              <button
                key={value}
                className={value === side ? "chip active" : "chip"}
                onClick={() => setSide(value)}
              >
                {value === "left"
                  ? "Izquierda"
                  : value === "right"
                    ? "Derecha"
                    : value === "top"
                      ? "Arriba"
                      : "Abajo"}
              </button>
            ))}
          </div>
          <button
            className="primary"
            disabled={busy || !remoteScreen}
            onClick={() => void saveLayout()}
          >
            {saved ? "Guardado ✓" : "Guardar posición"}
          </button>
          <p className="hint">
            {remoteScreen
              ? `El remoto quedará a la ${sideLabel[side]} de este equipo. Se aplica en la próxima sesión.`
              : "Añade una segunda pantalla al layout para poder colocarla."}
          </p>
        </article>

        <article className="card media">
          <header>
            <h2>Audio y vídeo</h2>
            <span className="badge muted-badge">M3 · M4</span>
          </header>
          <ul className="feature-list">
            <li>
              <div>
                <strong>Audio del sistema</strong>
                <span>Opus 48 kHz, datagramas QUIC, ~40 ms</span>
              </div>
              <span className="soon">Próximamente</span>
            </li>
            <li>
              <div>
                <strong>Segunda pantalla</strong>
                <span>Espejo primero; monitor virtual después</span>
              </div>
              <span className="soon">Próximamente</span>
            </li>
            <li>
              <div>
                <strong>Portapapeles</strong>
                <span>Texto, HTML e imágenes con hashes</span>
              </div>
              <span className="soon">Próximamente</span>
            </li>
          </ul>
        </article>
      </section>

      <footer>
        <span>
          Núcleo en Rust · QUIC + TLS 1.3 · <code>cargo test --workspace</code>
        </span>
        <span>M0 · build de desarrollo</span>
      </footer>
    </main>
  );
}
