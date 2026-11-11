// Shapes of the company and its numbering series.

export interface Company {
  nif: string
  name: string
  address: string
  postal_code: string
  city: string
}

export interface Series {
  code: string
  description: string
  active: boolean
  invoices: number
}
