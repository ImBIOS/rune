import { StrictMode } from "react"
import { createRoot, hydrateRoot } from "react-dom/client"
import { App } from "./App"
import "./system/tokens.css"
import "./site.css"

const root = document.getElementById("root")!
const page = (
  <StrictMode>
    <App />
  </StrictMode>
)

if (root.firstElementChild) hydrateRoot(root, page)
else createRoot(root).render(page)
