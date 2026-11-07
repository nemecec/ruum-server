import type { ReactNode } from 'react'

// The outline icons: 20 px grid, 1.5 px stroke (design-system 2.6).

export const iconPaths = {
  close: <path d="M5 5l10 10M15 5L5 15" />,
  plus: <path d="M10 4v12M4 10h12" />,
  minus: <path d="M4 10h12" />,
  check: <path d="M4 10.5l4 4 8-9" />,
  chevronDown: <path d="M5 8l5 5 5-5" />,
  chevronRight: <path d="M8 5l5 5-5 5" />,
  info: (
    <>
      <circle cx="10" cy="10" r="7.25" />
      <path d="M10 9v5M10 6.2v.1" />
    </>
  ),
  warning: (
    <>
      <path d="M10 3l7.5 13.5h-15z" strokeLinejoin="round" />
      <path d="M10 8.5v3.5M10 14.2v.1" />
    </>
  ),
  error: (
    <>
      <circle cx="10" cy="10" r="7.25" />
      <path d="M7.5 7.5l5 5M12.5 7.5l-5 5" />
    </>
  ),
  success: (
    <>
      <circle cx="10" cy="10" r="7.25" />
      <path d="M6.8 10.2l2.3 2.3 4.2-4.6" />
    </>
  ),
  copy: (
    <>
      <rect x="7" y="7" width="9.5" height="9.5" rx="1.5" />
      <path d="M13 7V5a1.5 1.5 0 0 0-1.5-1.5h-6A1.5 1.5 0 0 0 4 5v6a1.5 1.5 0 0 0 1.5 1.5H7" />
    </>
  ),
  menu: <path d="M3.5 6h13M3.5 10h13M3.5 14h13" />,
  more: (
    <>
      <circle cx="5" cy="10" r="0.6" />
      <circle cx="10" cy="10" r="0.6" />
      <circle cx="15" cy="10" r="0.6" />
    </>
  ),
  lock: (
    <>
      <rect x="4.5" y="9" width="11" height="8" rx="1.5" />
      <path d="M7 9V6.5a3 3 0 0 1 6 0V9" />
    </>
  ),
  person: (
    <>
      <circle cx="10" cy="7" r="3" />
      <path d="M4 17c.8-3.2 3.1-5 6-5s5.2 1.8 6 5" />
    </>
  ),
  hash: <path d="M8 3.5L6.5 16.5M13.5 3.5L12 16.5M4 7.5h12.5M3.5 12.5H16" />,
  search: (
    <>
      <circle cx="9" cy="9" r="5.25" />
      <path d="M13 13l4 4" />
    </>
  ),
  upload: <path d="M10 13V4M6 7.5L10 3.5l4 4M4 16.5h12" />,
  database: (
    <>
      <ellipse cx="10" cy="5" rx="6" ry="2.25" />
      <path d="M4 5v10c0 1.2 2.7 2.25 6 2.25s6-1 6-2.25V5M4 10c0 1.2 2.7 2.25 6 2.25s6-1 6-2.25" />
    </>
  ),
} satisfies Record<string, ReactNode>

export type IconName = keyof typeof iconPaths

export const iconNames = Object.keys(iconPaths) as IconName[]
