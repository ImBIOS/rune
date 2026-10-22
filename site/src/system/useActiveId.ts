import { useEffect, useState } from "react"

/** The id of the last element whose top has passed a line a third of the way down the viewport. Null on the server
 *  and during hydration, so the first client render matches the prerendered HTML. */
export function useActiveId(ids: string[]): string | null {
  const [active, setActive] = useState<string | null>(null)
  const key = ids.join(" ")

  useEffect(() => {
    if (ids.length === 0) return
    let frame = 0
    const measure = () => {
      frame = 0
      const line = Math.min(window.innerHeight * 0.33, 280)
      let current: string | null = null
      for (const id of ids) {
        const el = document.getElementById(id)
        if (el && el.getBoundingClientRect().top <= line) current = id
      }
      // At the very end of the page the last item wins, even if it never reaches the line.
      if (window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 2) {
        const last = [...ids].reverse().find(id => {
          const el = document.getElementById(id)
          return el && el.getBoundingClientRect().top < window.innerHeight
        })
        if (last) current = last
      }
      setActive(current)
    }
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(measure)
    }
    measure()
    window.addEventListener("scroll", schedule, { passive: true })
    window.addEventListener("resize", schedule)
    return () => {
      cancelAnimationFrame(frame)
      window.removeEventListener("scroll", schedule)
      window.removeEventListener("resize", schedule)
    }
  }, [key])

  return active
}
