const INVITE_VERSION = 2

export const NETWORK_INVITE_PREFIX = 'nvpn://invite/'

/**
 * @typedef {{
 *   v: number
 *   networkName: string
 *   networkId: string
 *   inviterNpub: string
 *   admins: string[]
 *   participants: string[]
 *   relays: string[]
 * }} InvitePayload
 */

/**
 * @typedef {{
 *   id: string
 *   name?: string
 *   networkId?: string
 *   enabled?: boolean
 *   participants?: unknown[]
 * }} InviteTargetNetwork
 */

const encodeBase64 = (value) =>
  typeof btoa === 'function'
    ? btoa(value)
    : Buffer.from(value, 'binary').toString('base64')

const decodeBase64 = (value) =>
  typeof atob === 'function'
    ? atob(value)
    : Buffer.from(value, 'base64').toString('binary')

const normalizeInviteString = (value, fieldLabel) => {
  const normalized = typeof value === 'string' ? value.trim() : ''
  if (!normalized) {
    throw new Error(`Invite ${fieldLabel} is empty`)
  }
  return normalized
}

const normalizeInvitePayload = (payload) => {
  if (!payload || typeof payload !== 'object') {
    throw new Error('Invite payload is invalid')
  }

  const version = Number(payload.v)
  if (!Number.isInteger(version)) {
    throw new Error('Invite version is invalid')
  }
  if (version !== 1 && version !== INVITE_VERSION) {
    throw new Error(`Unsupported invite version ${version}`)
  }

  const relays = Array.isArray(payload.relays)
    ? [...new Set(payload.relays.map((relay) => String(relay).trim()).filter(Boolean))]
    : []
  const inviterNpub = normalizeInviteString(
    payload.inviterNpub ?? payload.inviter_npub,
    'inviter pubkey',
  )
  const admins = Array.isArray(payload.admins)
    ? [...new Set(payload.admins.map((value) => String(value).trim()).filter(Boolean))]
    : []
  if (!admins.includes(inviterNpub)) {
    admins.push(inviterNpub)
  }
  const participants = Array.isArray(payload.participants)
    ? [...new Set(payload.participants.map((value) => String(value).trim()).filter(Boolean))]
    : [inviterNpub]

  return {
    v: version === 1 ? INVITE_VERSION : version,
    networkName: normalizeInviteString(
      payload.networkName ?? payload.network_name,
      'network name',
    ),
    networkId: normalizeInviteString(
      payload.networkId ?? payload.network_id,
      'network id',
    ),
    inviterNpub,
    admins,
    participants,
    relays,
  }
}

const decodeInviteText = (payload) => {
  if (payload.startsWith('{')) {
    return JSON.parse(payload)
  }

  const padded = payload + '='.repeat((4 - (payload.length % 4 || 4)) % 4)
  const binary = decodeBase64(padded.replace(/-/g, '+').replace(/_/g, '/'))
  const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0))
  return JSON.parse(new TextDecoder().decode(bytes))
}

export const normalizeInviteNetworkId = (value) => {
  const trimmed = typeof value === 'string' ? value.trim() : ''
  return trimmed.replace(/^nostr-vpn:/, '')
}

export const isPlaceholderNetworkName = (value) => {
  const trimmed = typeof value === 'string' ? value.trim() : ''
  return trimmed.length === 0 || /^Network \d+$/.test(trimmed)
}

/**
 * @param {InviteTargetNetwork | null | undefined} network
 */
export const networkShouldAdoptInvite = (network) =>
  !!network &&
  Array.isArray(network.participants) &&
  network.participants.length === 0 &&
  isPlaceholderNetworkName(network.name)

/**
 * @param {InviteTargetNetwork[]} networks
 * @param {string | null | undefined} activeNetworkId
 * @param {string} inviteNetworkId
 */
export const determineInviteImportTarget = (
  networks,
  activeNetworkId,
  inviteNetworkId,
) => {
  const normalizedInviteId = normalizeInviteNetworkId(inviteNetworkId)
  const existing = networks.find(
    (network) =>
      normalizeInviteNetworkId(String(network.networkId ?? '')) === normalizedInviteId,
  )
  if (existing) {
    return { mode: 'existing', networkId: existing.id }
  }

  const active =
    networks.find((network) => network.id === activeNetworkId) ??
    networks.find((network) => network.enabled) ??
    networks[0] ??
    null
  if (networkShouldAdoptInvite(active)) {
    return { mode: 'reuse-active', networkId: active.id }
  }

  return { mode: 'create', networkId: null }
}

/**
 * @param {InvitePayload} payload
 */
export const encodeInvitePayload = (payload) => {
  const normalized = normalizeInvitePayload(payload)
  const bytes = new TextEncoder().encode(JSON.stringify(normalized))
  let binary = ''
  for (const byte of bytes) {
    binary += String.fromCharCode(byte)
  }
  return `${NETWORK_INVITE_PREFIX}${encodeBase64(binary)
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/g, '')}`
}

/**
 * @param {string} invite
 * @returns {InvitePayload}
 */
export const decodeInvitePayload = (invite) => {
  const trimmed = invite.trim()
  if (!trimmed) {
    throw new Error('Invite code is empty')
  }

  const payload = trimmed.startsWith('{')
    ? trimmed
    : trimmed.startsWith(NETWORK_INVITE_PREFIX)
      ? trimmed.slice(NETWORK_INVITE_PREFIX.length)
      : trimmed

  return normalizeInvitePayload(decodeInviteText(payload))
}
