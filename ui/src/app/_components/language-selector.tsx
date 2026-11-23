"use client";
import {Languages} from 'lucide-react';
import {Select,SelectOption} from '@/components/ui/select';
import {useI18n,type Language} from '@/lib/i18n';

export function LanguageSelector() {
 const {language,setLanguage,t}=useI18n();
 return <Select aria-label={t('Langue de l’interface')} title={t('Langue de l’interface')} value={language} onValueChange={value=>setLanguage(value as Language)} icon={<Languages className="size-4 shrink-0" aria-hidden="true"/>}>
  <SelectOption value="fr" lang="fr">FR</SelectOption><SelectOption value="en" lang="en">EN</SelectOption>
 </Select>;
}
