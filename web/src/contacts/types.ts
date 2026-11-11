// Shapes of contacts.

export interface ContactInput {
  name: string
  nif: string
  address: string
  postal_code: string
  city: string
  email: string
  is_customer: boolean
  is_supplier: boolean
  archived: boolean
}

export interface Contact extends ContactInput {
  id: number
  invoices: number
}
