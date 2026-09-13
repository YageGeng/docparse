import { Link, Route, Routes } from "react-router";
import { lazy, Suspense } from "react";
import { FileStack } from "lucide-react";
import { JobsPage } from "@/features/jobs/JobsPage";
import { apiUrl } from "@/api/client";

// Keep PDF.js and the inspector out of the upload/history entry bundle.
const WorkspacePage = lazy(() =>
  import("@/features/viewer/WorkspacePage").then((module) => ({
    default: module.WorkspacePage,
  })),
);

/** Keeps shared navigation small so both history and document inspection devote their space to actual PDFs. */
export function App() {
  return (
    <>
      <a className="skip-link" href="#main-content">
        跳到主要内容
      </a>
      <header className="app-header">
        <Link to="/" className="brand">
          <span className="brand-mark">
            <FileStack size={21} />
          </span>
          <span>DocParse</span>
          <span className="brand-divider" />
          <span className="brand-caption">文档工作台</span>
        </Link>
        <nav aria-label="主导航">
          <Link to="/" className="nav-link">
            文档
          </Link>
          <a
            href={apiUrl("docs")}
            target="_blank"
            rel="noreferrer"
            className="nav-link"
          >
            API 文档
            <svg
              width="11"
              height="11"
              viewBox="0 0 12 12"
              fill="none"
              aria-hidden="true"
            >
              <path
                d="M3 9 9 3M3 3h6v6"
                stroke="currentColor"
                strokeWidth="1.4"
              />
            </svg>
          </a>
        </nav>
      </header>
      <Routes>
        <Route path="/" element={<JobsPage />} />
        <Route
          path="/document"
          element={
            <Suspense
              fallback={
                <main className="empty-state" id="main-content" role="status">
                  正在打开文档工作台…
                </main>
              }
            >
              <WorkspacePage />
            </Suspense>
          }
        />
        <Route
          path="*"
          element={
            <main className="empty-state" id="main-content">
              <h1>页面不存在</h1>
              <Link to="/">返回文档列表</Link>
            </main>
          }
        />
      </Routes>
    </>
  );
}
