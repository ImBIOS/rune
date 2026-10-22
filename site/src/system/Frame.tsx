import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { useActiveId } from "./useActiveId"
import "./primitives.css"

export type NavItem = {
  href: string
  label: string
  /** Mono counter shown before the label, e.g. a rule number. */
  num?: string
  /** Element id on this page that marks the item current while it is in view. */
  spy?: string
  /** The page this item opens is the one being shown. */
  current?: boolean
  children?: NavItem[]
  /** A quiet label above a run of children, e.g. a guide's section. */
  heading?: boolean
}

export type NavGroup = { label: string; items: NavItem[] }

function spyIds(items: NavItem[]): string[] {
  return items.flatMap(item => [...(item.spy ? [item.spy] : []), ...spyIds(item.children ?? [])])
}

function NavLinks({ items, active, depth }: { items: NavItem[]; active: string | null; depth: number }) {
  return (
    <ul className="nav-list" data-depth={depth}>
      {items.map(item =>
        item.heading ? (
          <li key={item.href} className="nav-heading">
            <a href={item.href}>{item.label}</a>
          </li>
        ) : (
          <li key={item.href}>
            <a
              href={item.href}
              className="nav-link"
              data-parent={(item.children && item.children.length > 0) || undefined}
              data-active={(item.spy ? item.spy === active : item.current) || undefined}
              aria-current={item.current && !item.children ? "page" : item.spy === active && item.spy ? "location" : undefined}
            >
              {item.num && <span className="nav-num">{item.num}</span>}
              <span className="nav-label">{item.label}</span>
            </a>
            {item.children && item.children.length > 0 && (
              <NavLinks items={item.children} active={active} depth={depth + 1} />
            )}
          </li>
        ),
      )}
    </ul>
  )
}

/** The persistent sidebar: brand, then groups of links. A current top-level item sits on a pill; a current child
 *  (a rule in view, a problem being read) lights its stretch of the guide line, and that mark travels as you scroll.
 *  The list fades at whichever edge has links scrolled under it. Under 900 px it folds into a top bar. */
function Sidebar({ brand, groups, revealBrandAfter }: { brand: ReactNode; groups: NavGroup[]; revealBrandAfter?: string }) {
  const ids = groups.flatMap(g => spyIds(g.items))
  const active = useActiveId(ids)
  const [open, setOpen] = useState(false)
  const [brandShown, setBrandShown] = useState(!revealBrandAfter)

  useEffect(() => {
    const hero = revealBrandAfter && document.getElementById(revealBrandAfter)
    if (!hero) return setBrandShown(true)
    const observer = new IntersectionObserver(([entry]) => setBrandShown(!entry!.isIntersecting))
    observer.observe(hero)
    return () => observer.disconnect()
  }, [revealBrandAfter])
  const scroller = useRef<HTMLDivElement>(null)
  const pill = useRef<HTMLSpanElement>(null)
  const rail = useRef<HTMLSpanElement>(null)

  const fade = () => {
    const el = scroller.current
    if (!el) return
    const below = el.scrollHeight - el.clientHeight - el.scrollTop
    el.style.setProperty("--fade-top", String(Math.min(1, el.scrollTop / 15)))
    el.style.setProperty("--fade-bottom", String(Math.min(1, Math.max(0, below) / 15)))
  }

  useLayoutEffect(() => {
    const el = scroller.current
    if (!el || !pill.current || !rail.current) return
    const markers = [pill.current, rail.current]
    const place = () => {
      // The deepest current leaf: the rule in view, not the guide that holds it.
      const links = el.querySelectorAll<HTMLElement>(".nav-link[data-active]:not([data-parent])")
      const link = links[links.length - 1]
      const list = link?.closest<HTMLElement>(".nav-list")
      const nested = !!list && list.dataset.depth !== "0"
      const shown = link ? (nested ? rail.current! : pill.current!) : null
      for (const marker of markers) {
        if (marker !== shown) {
          marker.style.opacity = "0"
          delete marker.dataset.placed
        }
      }
      if (!link || !shown || !list) return
      const box = el.getBoundingClientRect()
      const top = link.getBoundingClientRect().top - box.top + el.scrollTop
      const first = !shown.dataset.placed
      if (first) shown.style.transition = "none"
      // The rail mark is 2 px, centred on the 1 px guide line.
      const x = nested ? list.getBoundingClientRect().left - box.left - 0.5 : 0
      shown.style.transform = `translate(${x}px, ${top}px)`
      shown.style.height = `${link.offsetHeight}px`
      if (first) {
        void shown.offsetHeight
        shown.style.transition = ""
        shown.dataset.placed = ""
      }
      shown.style.opacity = "1"
      // Keep the current item inside the list's viewport, without animating the list.
      const linkTop = top - el.scrollTop
      if (linkTop < 24) el.scrollTop = top - 24
      else if (linkTop + link.offsetHeight > el.clientHeight - 24) el.scrollTop = top + link.offsetHeight - el.clientHeight + 24
      fade()
    }
    place()
    const observer = new ResizeObserver(place)
    observer.observe(el)
    return () => observer.disconnect()
  }, [active])

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false)
    document.addEventListener("keydown", onKey)
    return () => document.removeEventListener("keydown", onKey)
  }, [open])

  return (
    <aside className="sidebar" data-open={open || undefined}>
      <div className="sidebar-scrim" aria-hidden="true" onClick={() => setOpen(false)} />
      <div className="sidebar-brand" data-brand-hidden={!brandShown || undefined}>
        <div className="sidebar-lockup" inert={!brandShown}>
          {brand}
        </div>
        <button
          type="button"
          className="sidebar-toggle"
          aria-expanded={open}
          aria-controls="sidebar-nav"
          onClick={() => setOpen(o => !o)}
        >
          {open ? "Close" : "Menu"}
        </button>
      </div>
      <div
        ref={scroller}
        id="sidebar-nav"
        className="sidebar-scroll"
        onScroll={fade}
        onClick={e => (e.target as HTMLElement).closest("a") && setOpen(false)}
      >
        <span ref={pill} className="nav-pill" aria-hidden="true" />
        <span ref={rail} className="nav-rail" aria-hidden="true" />
        {groups.map(group => (
          <nav key={group.label} className="nav-group" aria-label={group.label}>
            <p className="nav-group-label">{group.label}</p>
            <NavLinks items={group.items} active={active} depth={0} />
          </nav>
        ))}
      </div>
    </aside>
  )
}

/** Page frame: sidebar on the left, a wide main column on the right, footer at the end of main. */
export function Frame({
  brand,
  groups,
  revealBrandAfter,
  footer,
  children,
}: {
  brand: ReactNode
  groups: NavGroup[]
  /** Id of a hero that already shows the brand: the sidebar's lockup fades in once it scrolls away. */
  revealBrandAfter?: string
  footer?: ReactNode
  children: ReactNode
}) {
  return (
    <div className="frame">
      <Sidebar brand={brand} groups={groups} revealBrandAfter={revealBrandAfter} />
      <div className="frame-main">
        <main className="frame-content">{children}</main>
        {footer}
      </div>
    </div>
  )
}

/** Eyebrow, title and lede at the top of a page. */
export function PageHead({ eyebrow, title, children }: { eyebrow?: ReactNode; title: ReactNode; children?: ReactNode }) {
  return (
    <header className="page-head">
      {eyebrow && <p className="eyebrow">{eyebrow}</p>}
      <h1 className="page-title">{title}</h1>
      {children && <div className="page-lede">{children}</div>}
    </header>
  )
}

/** A section's heading on a hairline, with an optional count or note on the right. */
export function SectionHead({ id, title, aside, children }: { id?: string; title: ReactNode; aside?: ReactNode; children?: ReactNode }) {
  return (
    <header className="section-head">
      <div className="section-head-row">
        <h2 id={id}>{title}</h2>
        {aside && <span className="section-aside">{aside}</span>}
      </div>
      {children && <div className="section-intro">{children}</div>}
    </header>
  )
}
