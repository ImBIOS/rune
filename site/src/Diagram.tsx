/** The architecture in one figure: the app side and the render thread, the command channel between them, and the
 *  platform shell under both. Plain HTML so the text stays crisp and the layout can stack on a phone. */
export function ArchitectureDiagram() {
  return (
    <figure className="arch" aria-labelledby="arch-caption">
      <div className="arch-grid">
        <section className="arch-box" data-area="app">
          <p className="arch-kicker">Main thread</p>
          <h3>App side</h3>
          <ul>
            <li>Models: typed state, keyed by stable ids</li>
            <li>View functions → keyed tree → diff</li>
            <li>Taffy layout at the target state</li>
            <li>Parley shaping and line cache</li>
            <li>Handlers, queued between frames</li>
            <li>Accessibility tree updates</li>
          </ul>
        </section>

        <div className="arch-channel" data-area="channel">
          <p className="arch-channel-label">Command channel</p>
          <div className="arch-flow" data-dir="fwd">
            <span className="arch-arrow" aria-hidden="true" />
            <span className="arch-flow-text">tree diffs, target rects, text runs, spring targets</span>
          </div>
          <div className="arch-flow" data-dir="back">
            <span className="arch-arrow" aria-hidden="true" />
            <span className="arch-flow-text">hits, gestures, settles, frame ids</span>
          </div>
        </div>

        <section className="arch-box" data-area="render">
          <p className="arch-kicker">Paced by the display</p>
          <h3>Render thread</h3>
          <ul>
            <li>Presented tree: what's on screen now</li>
            <li>Spring tracks that retarget with velocity</li>
            <li>Exiting nodes, edge-by-edge layout motion</li>
            <li>Scroll physics, drags applied 1:1</li>
          </ul>
          <div className="arch-inner">
            <p className="arch-inner-title">Renderer · wgpu</p>
            <ul>
              <li>One instanced pass: rects, shadows, glyphs, vectors</li>
              <li>Soft-edge clip on every instance</li>
            </ul>
          </div>
        </section>

        <section className="arch-box arch-shell" data-area="shell">
          <div>
            <p className="arch-kicker">Per platform</p>
            <h3>Shell</h3>
            <p className="arch-line">Windows and panels, menus, clipboard, text input protocol, accessibility adapter, lifecycle</p>
          </div>
          <div data-optional>
            <p className="arch-kicker">Apple, optional</p>
            <h3>Native layers</h3>
            <p className="arch-line">System glass and video behind Rune's layer, masked by the render thread each frame</p>
          </div>
        </section>
      </div>
      <figcaption id="arch-caption">
        The app side states targets and the render thread animates toward them. Everything that crosses between them
        goes through the command channel.
      </figcaption>
    </figure>
  )
}
