// Corrected types of the upload schemas.
//
// `CreateUploadRequest` and `Upload` are `allOf: [UploadTarget, {...}]`, and `UploadTarget` has a
// discriminator. openapi-typescript (7.x) renders that as `{ target: "CreateUploadRequest" } &
// Omit<UploadTarget, "target">`, which is wrong: the real `target` is `new_file` or `new_version`,
// and `Omit` of a union drops the fields of each branch. These types say what the contract says.
// They are built from the generated parts, so a contract change still breaks the build.

import type { Schemas } from './client'

type Generated<T> = Omit<T, 'target'>

/** Where an upload goes (`UploadTarget`): a new file in a folder, or a new version of a file. */
export type UploadTarget = Schemas['UploadTargetNewFile'] | Schemas['UploadTargetNewVersion']

/** The body of `createUpload`. */
export type CreateUploadRequest = UploadTarget & Generated<Schemas['CreateUploadRequest']>

/** An upload session. */
export type Upload = UploadTarget & Generated<Schemas['Upload']>

/** The generated type that the typed client wants for a `createUpload` body. */
export function asCreateUploadBody(body: CreateUploadRequest): Schemas['CreateUploadRequest'] {
  return body as unknown as Schemas['CreateUploadRequest']
}

/** Reads an `Upload` answer with the corrected type. */
export function asUpload(upload: Schemas['Upload']): Upload {
  return upload as unknown as Upload
}
