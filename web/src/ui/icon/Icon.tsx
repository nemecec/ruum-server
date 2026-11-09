import { cx } from '../cx'
import { iconPaths as paths, type IconName } from './paths'
import './icon.css'

// Outline icons on a 20 px grid (design-system 2.6), drawn here; the stroke width, caps and joins
// come from the icon.* tokens through icon.css (the attributes below are the fallback). The set is
// small, so no icon dependency. Icons are decorative: the text or `aria-label` beside them names
// the action.

export type { IconName }

/** One outline icon, 20 px (16 px with `small`). */
export function Icon({ name, small }: { name: IconName; small?: boolean }) {
  return (
    <svg
      className={cx('ui-icon', small && 'ui-icon-sm')}
      viewBox="0 0 20 20"
      aria-hidden="true"
      focusable="false"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
    >
      {paths[name]}
    </svg>
  )
}

/** The glyph of a folder (an outline with a tab). Decorative. */
export function FolderIcon() {
  return (
    <svg className="ui-icon ui-kind-folder" viewBox="0 0 20 20" aria-hidden="true" focusable="false">
      <path
        d="M2.5 5.5a1 1 0 0 1 1-1h4l1.6 1.8h7.4a1 1 0 0 1 1 1v8.2a1 1 0 0 1-1 1h-13a1 1 0 0 1-1-1z"
        fill="currentColor"
        opacity="0.85"
      />
    </svg>
  )
}

const imageName = /\.(png|jpe?g|gif|webp|avif|heic|bmp|tiff?|svg)$/i

/**
 * The glyph of a file (a page with a folded corner and lines), or of a picture (a frame with a
 * hill and a sun) for images, so photos and notes differ at a glance. Decorative.
 */
export function FileIcon({ type, name }: { type?: string | null; name?: string } = {}) {
  if (type?.startsWith('image/') || (name && imageName.test(name))) {
    return (
      <svg className="ui-icon ui-kind-file" viewBox="0 0 20 20" aria-hidden="true" focusable="false">
        <rect x="2.5" y="4" width="15" height="12" rx="1.2" fill="none" stroke="currentColor" strokeWidth="1.4" />
        <path d="M4.5 14l3.6-4.2 2.6 2.8 1.8-1.9 3 3.3" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round" />
        <circle cx="13.3" cy="7.6" r="1.3" fill="currentColor" />
      </svg>
    )
  }
  return (
    <svg className="ui-icon ui-kind-file" viewBox="0 0 20 20" aria-hidden="true" focusable="false">
      <path d="M5 2.5h6.5L15.5 6.5v11H5z" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" />
      <path d="M11.5 2.5v4h4M7.5 10h5M7.5 12.8h5" fill="none" stroke="currentColor" strokeWidth="1.2" />
    </svg>
  )
}

/** The icon of an item by its kind: a folder, a picture or a page. */
export function KindIcon({ kind, type, name }: { kind: string; type?: string | null; name?: string }) {
  return kind === 'folder' ? <FolderIcon /> : <FileIcon type={type} name={name} />
}

/**
 * The agent mark (design-system 2.6): a solid rounded square with a small key-shaped cut, in the
 * agent colour. Never animated; never a robot, a sparkle or a wand. Decorative: the text beside it
 * says "agent".
 */
export function AgentMark() {
  return (
    <svg className="ui-agent-mark" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        fillRule="evenodd"
        fill="currentColor"
        d="M4 1.5h8A2.5 2.5 0 0 1 14.5 4v8a2.5 2.5 0 0 1-2.5 2.5H4A2.5 2.5 0 0 1 1.5 12V4A2.5 2.5 0 0 1 4 1.5zM6.2 5a2 2 0 1 0 1.9 2.6h1.4v1.2h1.2V7.6h.8V6.4H8.1A2 2 0 0 0 6.2 5zm0 1.2a.8.8 0 1 1 0 1.6.8.8 0 0 1 0-1.6z"
      />
    </svg>
  )
}
