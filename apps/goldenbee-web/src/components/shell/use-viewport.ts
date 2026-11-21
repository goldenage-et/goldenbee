import { useEffect, useState } from "react"

/** Viewport query for the drawer/single-column breakpoints. */
export function useViewportBelow(pixels: number): boolean {
  const [below, setBelow] = useState(() =>
    typeof window === "undefined" ? false : window.innerWidth < pixels,
  )

  useEffect(() => {
    function measure(): void {
      setBelow(window.innerWidth < pixels)
    }
    measure()
    window.addEventListener("resize", measure)
    return () => {
      window.removeEventListener("resize", measure)
    }
  }, [pixels])

  return below
}
