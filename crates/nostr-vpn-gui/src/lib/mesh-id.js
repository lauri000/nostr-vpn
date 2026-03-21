export const MESH_ID_COMPAT_PREFIX = 'nostr-vpn:'
export const MESH_ID_LEGACY_DEFAULT = 'nostr-vpn'

const COMPACT_MESH_ID_PATTERN = /^[A-Za-z0-9]+$/
const HYPHENATED_MESH_ID_PATTERN = /^[A-Za-z0-9]+(?:-[A-Za-z0-9]+)*$/

const chunkMeshId = (value) => value.match(/.{1,4}/g)?.join('-') ?? value

export const meshIdUsesCompatPrefix = (value) => value.trim().startsWith(MESH_ID_COMPAT_PREFIX)

export const stripMeshIdPrefix = (value) => {
  const trimmed = value.trim()
  if (!meshIdUsesCompatPrefix(trimmed)) {
    return trimmed
  }

  return trimmed.slice(MESH_ID_COMPAT_PREFIX.length)
}

export const formatMeshIdForDisplay = (value) => {
  const trimmed = value.trim()
  if (!trimmed || trimmed === MESH_ID_LEGACY_DEFAULT) {
    return trimmed
  }

  const visible = stripMeshIdPrefix(trimmed)
  if (!COMPACT_MESH_ID_PATTERN.test(visible) || visible.length <= 4) {
    return visible
  }

  return chunkMeshId(visible)
}

export const canonicalizeMeshIdInput = (value, currentMeshId = '') => {
  const trimmed = value.trim()
  if (!trimmed) {
    return ''
  }

  const currentTrimmed = currentMeshId.trim()
  if (trimmed === formatMeshIdForDisplay(currentTrimmed)) {
    return currentTrimmed
  }

  if (meshIdUsesCompatPrefix(trimmed)) {
    return trimmed
  }

  if (meshIdUsesCompatPrefix(currentTrimmed)) {
    const compact = trimmed.replace(/-/g, '')
    if (COMPACT_MESH_ID_PATTERN.test(compact) && compact.length >= 8 && compact.length <= 24) {
      return `${MESH_ID_COMPAT_PREFIX}${compact}`
    }
  }

  return trimmed
}

export const validateMeshIdInput = (value, currentMeshId = '') => {
  const trimmed = value.trim()
  if (!trimmed) {
    return 'Mesh ID cannot be empty.'
  }

  const canonical = canonicalizeMeshIdInput(trimmed, currentMeshId)
  if (canonical === MESH_ID_LEGACY_DEFAULT) {
    return ''
  }

  if (meshIdUsesCompatPrefix(canonical)) {
    const suffix = canonical.slice(MESH_ID_COMPAT_PREFIX.length)
    if (!suffix) {
      return 'Mesh ID cannot be empty.'
    }
    if (!COMPACT_MESH_ID_PATTERN.test(suffix)) {
      return 'Use only letters and numbers after the hidden app prefix.'
    }
    if (suffix.length < 8 || suffix.length > 24) {
      return 'Use 8 to 24 letters or numbers.'
    }
    return ''
  }

  if (!HYPHENATED_MESH_ID_PATTERN.test(canonical)) {
    return 'Use only letters, numbers, and hyphens.'
  }

  const groups = canonical.split('-')
  if (groups.some((group) => group.length === 0 || group.length > 4)) {
    return 'Use groups of up to 4 characters.'
  }
  if (canonical.includes('-') && groups.some((group) => group.length !== 4)) {
    return 'Use 4-character groups, like abcd-efgh-ijkl.'
  }

  const compact = canonical.replace(/-/g, '')
  if (compact.length < 8 || compact.length > 24) {
    return 'Use 8 to 24 letters or numbers total.'
  }

  return ''
}
