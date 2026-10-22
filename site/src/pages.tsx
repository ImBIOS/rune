import type { ReactNode } from "react"
import { decisions, layers, milestones, principles, risks, type Entry, type Milestone } from "./content"
import { ArchitectureDiagram } from "./Diagram"
import { withBase } from "./base"
import { REPO } from "./routes"
import { PageHead, SectionHead } from "./system/Frame"

export const num = (i: number) => String(i + 1).padStart(2, "0")

function Rule({ entry, number, children }: { entry: Entry; number?: string; children?: ReactNode }) {
  return (
    <article id={entry.slug} className="rule">
      <header className="rule-head">
        {number && (
          <a className="rule-num" href={`#${entry.slug}`} aria-label={`${entry.title}, ${number}`}>
            {number}
          </a>
        )}
        <h3 className="rule-title">{entry.title}</h3>
      </header>
      <div className="rule-body">
        <div className="rule-text">{entry.body}</div>
        {entry.spec && (
          <dl className="rule-spec">
            {entry.spec.map(s => (
              <div key={s.label}>
                <dt>{s.label}</dt>
                <dd className={s.mono ? "mono" : undefined}>
                  {s.values.map((v, i) => (
                    <span key={i}>{v}</span>
                  ))}
                </dd>
              </div>
            ))}
          </dl>
        )}
      </div>
      {children}
    </article>
  )
}

function Pager({ prev, next }: { prev?: [string, string]; next?: [string, string] }) {
  return (
    <nav className="doc-pager" aria-label="Pages">
      {prev ? <a href={withBase(prev[0])}>← {prev[1]}</a> : <span />}
      {next && <a href={withBase(next[0])}>{next[1]} →</a>}
    </nav>
  )
}

const running = [
  {
    title: "Settings window",
    text: "Toggles, a segmented control, a slider, and theme and accent changes that animate on springs.",
    meta: "examples/settings",
  },
  {
    title: "Component gallery",
    text: "One scene per @opencode/ui story, rendered on the OpenCode theme and diffed against the web version.",
    meta: "examples/kit-gallery",
  },
  {
    title: "OpenCode Desktop",
    text: "The home and session pages, from a fixture or read-only from a live OpenCode server.",
    meta: "examples/opencode",
  },
  {
    title: "Effects",
    text: "Glow, pulses, fire, distortion and motion blur as GPU layers on any element's shape.",
    meta: "crates/rune-fx",
  },
  {
    title: "Headless renders",
    text: "Every example renders offscreen to a PNG at a chosen moment, for goldens and for agents.",
    meta: "--snap",
  },
]

export function HomeView() {
  return (
    <>
      <PageHead eyebrow={<><span>Rust UI framework</span><span>Early</span></>} title="Rune">
        <p>
          Rune is a Rust UI framework with its own GPU renderer, text, layout, spring animation and thin shells for each
          platform. It aims to feel like SwiftUI on a 120&nbsp;Hz iPhone on every platform, and to be written and checked by
          coding agents.
        </p>
        <p>
          The code and the full plan are at <a href={REPO}>anomalyco/rune</a>.
        </p>
      </PageHead>

      <section className="home-section">
        <SectionHead id="status" title="Status" aside="October 2026">
          <p>
            Rune is early, and its APIs will change. Today it runs on macOS through winit and wgpu, and draws each frame
            in one draw call.
          </p>
        </SectionHead>
        <ul className="index-list">
          {running.map(r => (
            <li key={r.title} className="index-row">
              <span className="index-title">{r.title}</span>
              <p className="index-text">{r.text}</p>
              <span className="index-meta">{r.meta}</span>
            </li>
          ))}
        </ul>
        <div className="shots">
          <figure className="shot">
            <img src={withBase("/shots/settings.png")} alt="A dark Settings window with theme, accent, toggle and slider rows" width={1240} height={1040} loading="lazy" />
            <figcaption>examples/settings, rendered headless</figcaption>
          </figure>
          <figure className="shot">
            <img src={withBase("/shots/opencode-session.png")} alt="OpenCode Desktop's session page: a project sidebar, a transcript with code, and the composer" width={1600} height={1025} loading="lazy" />
            <figcaption>examples/opencode, session page from the fixture</figcaption>
          </figure>
        </div>
      </section>

      <section className="home-section">
        <SectionHead id="next" title="Next">
          <p>
            The first real screen is Hex's Settings on Mac, Windows and Linux, which brings Rune's own platform shells
            and native text input. After that come OpenCode Desktop's main screens. That's the flagship test: it should
            look like the current app side by side and feel clearly better.
          </p>
        </SectionHead>
      </section>

      <Pager next={["/principles", "Principles"]} />
    </>
  )
}

export function PrinciplesView() {
  return (
    <>
      <PageHead eyebrow={<><span>Principles</span><span>{principles.length}</span></>} title="Principles">
        <p>
          What Rune optimises for, most important first. Where it can, the framework enforces each one so that apps get
          it by default.
        </p>
      </PageHead>
      <div className="rules">
        {principles.map((p, i) => (
          <Rule key={p.slug} entry={p} number={num(i)} />
        ))}
      </div>
      <Pager prev={["/", "Overview"]} next={["/architecture", "Architecture"]} />
    </>
  )
}

const rustToggle = `fn toggle(p: Paint, on: bool) -> El {
    let (w, h, k) = (38.0, 22.0, 18.0);
    let inset = (h - k) / 2.0;
    el().key("switch")
        .role("switch")
        .size(w, h)
        .radius(h / 2.0)
        .fill(if on { p.accent } else { p.t.control_off })
        .motion(motion::SNAPPY)
        .child(
            el().key("knob")
                .size(k, k)
                .absolute(if on { w - k - inset } else { inset }, inset)
                .radius(k / 2.0)
                .fill(p.t.knob),
        )
}`

export function ArchitectureView() {
  return (
    <>
      <PageHead eyebrow={<span>Architecture</span>} title="How Rune is built">
        <p>
          A Rune app runs on two threads. The app side turns state into a keyed tree and lays it out at its target
          state. The render thread owns what's on screen and animates toward those targets. Everything between them
          goes through one command channel.
        </p>
      </PageHead>

      <section className="doc-section">
        <SectionHead id="overview" title="Overview" />
        <ArchitectureDiagram />
      </section>

      <section className="doc-section">
        <SectionHead id="channel" title="The command channel" />
        <div className="prose">
          <p>
            The channel carries tree diffs, target layout rects, shaped text runs and spring targets to the render
            thread, and hits, gesture results, settles and presented-frame ids back. It's a real, serialisable protocol
            rather than shared memory, and three things follow from that.
          </p>
          <ul>
            <li>
              <strong>Animations survive a busy app.</strong> When a value animates, the app side does no view re-run,
              layout or diff.
            </li>
            <li>
              <strong>Hot reload reaches the iPhone.</strong> In development the channel runs over USB, with the render
              thread on the phone and the app side on the Mac, where it can be hot-patched.
            </li>
            <li>
              <strong>Agents and tests read the same stream.</strong> Recording the channel gives deterministic replay,
              frame traces and an inspector.
            </li>
          </ul>
        </div>
      </section>

      <section className="doc-section">
        <SectionHead id="layers" title="Layers" aside={`01–${num(layers.length - 1)}`} />
        <div className="rules">
          {layers.map((l, i) => (
            <Rule key={l.slug} entry={l} number={num(i)}>
              {l.slug === "tree" && (
                <pre className="code">
                  <code>{rustToggle}</code>
                </pre>
              )}
            </Rule>
          ))}
        </div>
      </section>

      <div id="choices">
      <section className="doc-section">
        <SectionHead id="decisions" title="Decisions">
          <p>What Rune chose for each layer, what it turned down, and why.</p>
        </SectionHead>
        <div className="table-wrap">
          <table className="decisions">
            <thead>
              <tr>
                <th scope="col">Area</th>
                <th scope="col">Chose</th>
                <th scope="col">Instead of</th>
                <th scope="col">Why</th>
              </tr>
            </thead>
            <tbody>
              {decisions.map(d => (
                <tr key={d.area}>
                  <th scope="row">{d.area}</th>
                  <td data-label="Chose">{d.chose}</td>
                  <td className="rejected" data-label="Instead of">{d.rejected}</td>
                  <td>{d.why}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>

      <section className="doc-section">
        <SectionHead id="gpui" title="Why not fork GPUI" />
        <div className="prose">
          <p>
            Zed's GPUI is the closest existing framework, and it has gained a wgpu backend, iOS and web shells, springs,
            AccessKit and hot reload. It still redraws the whole window on every dirty frame, has no layer compositing
            or group opacity, and runs animations by re-rendering views on the main thread. Those are architectural, and
            they're the reasons Kit's apps kept leaving it for Apple frameworks.
          </p>
          <p>
            So Rune starts fresh and borrows GPUI's best parts under its Apache-2.0 licence: the rope and sum tree, the
            line layout cache, the macOS and iOS text input code and the web textarea bridge.
          </p>
        </div>
      </section>
      </div>

      <Pager prev={["/principles", "Principles"]} next={["/roadmap", "Roadmap"]} />
    </>
  )
}

const STATUS_LABEL: Record<Milestone["status"], string> = {
  done: "Done",
  now: "Now",
  started: "Started",
  next: "Next",
  later: "Later",
}

export function RoadmapView() {
  return (
    <>
      <PageHead eyebrow={<span>Roadmap</span>} title="Roadmap">
        <p>
          Rune ships one real surface at a time, and each one replaces the code it was built for. The order puts the
          three desktops together first, because one wgpu backend covers them.
        </p>
      </PageHead>

      <section className="doc-section">
        <SectionHead id="milestones" title="Milestones" />
        <ol className="index-list milestones">
          {milestones.map(m => (
            <li key={m.id} id={m.id} className="index-row">
              <span className="index-title">
                <span className="milestone-id">{m.id.toUpperCase().replace("-", ".")}</span>
                {m.title}
              </span>
              <div className="index-text">{m.body}</div>
              <span className="index-meta">
                <span className="status" data-status={m.status}>
                  {STATUS_LABEL[m.status]}
                </span>
              </span>
            </li>
          ))}
        </ol>
      </section>

      <section className="doc-section">
        <SectionHead id="risks" title="Risks" />
        <div className="rules">
          {risks.map(r => (
            <Rule key={r.slug} entry={r} />
          ))}
        </div>
      </section>

      <Pager prev={["/architecture", "Architecture"]} />
    </>
  )
}

export function NotFoundView() {
  return (
    <PageHead title="Not found">
      <p>
        That page doesn't exist. Go to the <a href={withBase("/")}>overview</a>.
      </p>
    </PageHead>
  )
}
