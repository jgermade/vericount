// API endpoints for contacts.

import { request } from "../app/http"
import type { Contact, ContactInput } from "./types"

export const contactsApi = {
  listContacts: () => request<Contact[]>("GET", "/contacts"),
  createContact: (contact: ContactInput) => request<Contact>("POST", "/contacts", contact),
  updateContact: (id: number, contact: ContactInput) =>
    request<Contact>("PUT", `/contacts/${id}`, contact),
}
