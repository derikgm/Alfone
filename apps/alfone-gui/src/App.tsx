import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
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

type DaemonSession = {
  role: string;
  state: number;
  pin: string | null;
};

type LocalSession = {
  running: boolean;
  role: string | null;
  pid: number | null;
  exitCode: number | null;
};

type SessionInfo = {
  daemon: DaemonSession | null;
  local: LocalSession;
  logPath: string;
  binary: string;
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

const demoSession: SessionInfo = {
  daemon: null,
  local: { running: false, role: null, pid: null, exitCode: null },
  logPath: "~/.config/alfone/Alfone/alfone-session.log",
  binary: "alfone",
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
  const [session, setSession] = useState<SessionInfo | null>(null);
  const [layout, setLayout] = useState<Layout | null>(null);
  const [pin, setPin] = useState<string>("");
  const [clientAddress, setClientAddress] = useState<string>("");
  const [clientFingerprint, setClientFingerprint] = useState<string>("");
  const [side, setSide] = useState<Side>("right");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const canvasRef = useRef<HTMLDivElement>(null);
  const localRef = useRef<HTMLDivElement>(null);
  const remoteRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ x: number; y: number; dx: number; dy: number } | null>(
    null,
  );
  const [drag, setDrag] = useState<{ dx: number; dy: number } | null>(null);

  const refresh = useCallback(async () => {
    try {
      if (!isTauri()) {
        setStatus(demoStatus);
        setHealth({ ok: true, state: 0 });
        setSession(demoSession);
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
      // Session info succeeds even when the daemon is down: it reports the
      // child PID held by the panel (or that no session exists).
      try {
        const info = await invoke<SessionInfo>("session_info");
        if (!cancelled) {
          setSession(info);
        }
      } catch {
        if (!cancelled) {
          setSession(null);
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

  const startServer = async () => {
    setBusy(true);
    try {
      if (!isTauri()) {
        setSession({
          ...demoSession,
          daemon: { role: "server", state: 0, pin: pin || "123456" },
          local: { running: true, role: "server", pid: 4242, exitCode: null },
        });
      } else {
        setSession(
          await invoke<SessionInfo>("start_server", {
            pin: pin || null,
            side,
          }),
        );
      }
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const startClient = async () => {
    setBusy(true);
    try {
      if (!isTauri()) {
        setSession({
          ...demoSession,
          local: { running: true, role: "client", pid: 4243, exitCode: null },
        });
      } else {
        setSession(
          await invoke<SessionInfo>("start_client", {
            server: clientAddress,
            pin,
            fingerprint: clientFingerprint || null,
          }),
        );
      }
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const stopSession = async () => {
    setBusy(true);
    try {
      setSession(
        isTauri()
          ? await invoke<SessionInfo>("stop_session")
          : demoSession,
      );
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
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

  // Once the daemon is up, its own PIN wins: it may have generated one when
  // the panel started `serve` without an explicit PIN.
  const activePin = session?.daemon?.pin || pin;
  const daemonOnline = Boolean(session?.daemon || health);
  const sessionActive = Boolean(session?.local.running || daemonOnline);
  const serverCommand =
    activePin.length > 0 ? `alfone serve --pin ${activePin}` : "alfone serve";

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

  const saveLayout = async (override?: Side) => {
    if (!layout || !localScreen || !remoteScreen) {
      return;
    }
    const chosen = override ?? side;
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
        return link(screen, chosen, remoteScreen.id);
      }
      if (screen.id === remoteScreen.id) {
        return link(screen, opposite[chosen], localScreen.id);
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
      setSide(chosen);
      setSaved(true);
      setError(null);
      window.setTimeout(() => setSaved(false), 1800);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // Drag the remote screen on the canvas. On drop, snap it to the side of the
  // local screen it was moved towards and persist the new neighbour links.
  const startDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) {
      return;
    }
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = { x: event.clientX, y: event.clientY, dx: 0, dy: 0 };
    setDrag({ dx: 0, dy: 0 });
  };

  const moveDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    const origin = dragRef.current;
    if (!origin) {
      return;
    }
    origin.dx = event.clientX - origin.x;
    origin.dy = event.clientY - origin.y;
    setDrag({ dx: origin.dx, dy: origin.dy });
  };

  const endDrag = (_event: ReactPointerEvent<HTMLDivElement>) => {
    const origin = dragRef.current;
    dragRef.current = null;
    setDrag(null);
    if (!origin || !localRef.current || !remoteRef.current) {
      return;
    }
    if (Math.hypot(origin.dx, origin.dy) < 14) {
      return;
    }
    // The remote element still has the drag transform applied here, so its
    // bounding box is the position the user actually dropped it at.
    const localBox = localRef.current.getBoundingClientRect();
    const remoteBox = remoteRef.current.getBoundingClientRect();
    const dx =
      remoteBox.left + remoteBox.width / 2 - (localBox.left + localBox.width / 2);
    const dy =
      remoteBox.top + remoteBox.height / 2 - (localBox.top + localBox.height / 2);
    const next: Side =
      Math.abs(dx) >= Math.abs(dy)
        ? dx >= 0
          ? "right"
          : "left"
        : dy >= 0
          ? "bottom"
          : "top";
    if (next !== side) {
      void saveLayout(next);
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

        <article className="card session">
          <header>
            <h2>Sesión</h2>
            <span
              className={
                sessionActive ? "badge active-badge" : "badge muted-badge"
              }
            >
              {sessionActive ? "en marcha" : "detenida"}
            </span>
          </header>
          <p className="muted">
            El daemon posee el teclado y la sesión QUIC. Esta ventana solo lanza
            y detiene el proceso.
          </p>
          <div className="session-facts">
            <div>
              <span>Rol</span>
              <strong>
                {session?.local.role ?? session?.daemon?.role ?? "—"}
              </strong>
            </div>
            <div>
              <span>PID</span>
              <strong className="mono">{session?.local.pid ?? "—"}</strong>
            </div>
            <div>
              <span>Estado</span>
              <strong>
                {session?.daemon
                  ? session.daemon.state === 1
                    ? "activa"
                    : "en espera"
                  : session?.local.running
                    ? "conectando"
                    : "—"}
              </strong>
            </div>
          </div>
          {!daemonOnline && (
            <>
              <div className="field">
                <label htmlFor="server-address">Servidor al que conectar</label>
                <input
                  id="server-address"
                  className="mono"
                  value={clientAddress}
                  onChange={(event) => setClientAddress(event.target.value)}
                  placeholder="192.168.1.20:24800"
                />
              </div>
              <div className="field">
                <label htmlFor="server-fingerprint">
                  Huella esperada (opcional)
                </label>
                <input
                  id="server-fingerprint"
                  className="mono"
                  value={clientFingerprint}
                  onChange={(event) => setClientFingerprint(event.target.value)}
                  placeholder="Se fija en el primer emparejamiento"
                />
              </div>
            </>
          )}
          <div className="actions">
            <button
              className="primary"
              disabled={busy || sessionActive}
              onClick={() => void startServer()}
            >
              Iniciar servidor
            </button>
            <button
              className="ghost"
              disabled={
                busy || sessionActive || clientAddress.trim().length === 0
              }
              onClick={() => void startClient()}
            >
              Conectar cliente
            </button>
            <button
              className="ghost danger"
              disabled={busy || !sessionActive}
              onClick={() => void stopSession()}
            >
              Detener
            </button>
          </div>
          {daemonOnline && (
            <p className="hint">
              El PIN real del daemon se muestra abajo para emparejar el otro
              equipo.
            </p>
          )}
          <p className="hint small">
            Registro: <code>{session?.logPath ?? "…"}</code>
          </p>
        </article>

        <article className="card pairing">
          <header>
            <h2>Emparejar</h2>
            <span className="badge">PIN por sesión</span>
          </header>
          <p className="muted">
            Arranca el servidor en el equipo que tiene el teclado. Si dejas el
            PIN vacío, el daemon genera uno y la ventana muestra el real.
          </p>
          <div className="pin-row">
            <div className="pin">{activePin || "••••••"}</div>
            <button
              className="ghost"
              disabled={busy || daemonOnline}
              onClick={() => void generatePin()}
            >
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
            <code>
              alfone connect --server IP:24800 --pin {activePin || "PIN"}
            </code>
          </p>
        </article>

        <article className="card layout">
          <header>
            <h2>Pantallas</h2>
            <span className="badge">
              {layout ? `${layout.screens.length} pantalla(s)` : "KVM"}
            </span>
          </header>
          <div
            ref={canvasRef}
            className={`canvas side-${side}${drag ? " dragging" : ""}`}
          >
            <div ref={localRef} className="screen local">
              <span>{localScreen?.name ?? "Este equipo"}</span>
              <small>
                {localScreen
                  ? `${localScreen.width}×${localScreen.height}`
                  : "…"}
              </small>
            </div>
            <div
              ref={remoteRef}
              className="screen remote"
              style={
                drag
                  ? {
                      transform: `translate(${drag.dx}px, ${drag.dy}px)`,
                      zIndex: 2,
                    }
                  : undefined
              }
              onPointerDown={startDrag}
              onPointerMove={moveDrag}
              onPointerUp={endDrag}
              onPointerCancel={endDrag}
            >
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
              ? `Arrastra la pantalla remota para colocarla. Ahora está a la ${sideLabel[side]} de este equipo; también puedes elegir el lado con los botones.`
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
